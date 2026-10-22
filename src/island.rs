//! The notch: its outline, collapsed/expanded state, animation, and which
//! part of the window accepts input.

use std::cell::{Cell, RefCell};
use std::f64::consts::FRAC_PI_2;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, Align, DrawingArea, Stack, StackTransitionType};

use crate::cards::clock::{Collapsed, Idle};
use crate::cards::music::{Lyrics, MusicCard};
use crate::cards::peek::{Peek, PeekCard};
use crate::services::lyrics;
use crate::services::mpris::{Command, Update};

/// Body sizes (without the ears) for each state.
pub const COLLAPSED: (f64, f64) = (200.0, 32.0);
pub const EXPANDED_MUSIC: (f64, f64) = (520.0, 196.0);
pub const EXPANDED_LYRICS: (f64, f64) = (820.0, 196.0);
pub const EXPANDED_IDLE: (f64, f64) = (300.0, 118.0);
pub const PEEK: (f64, f64) = (400.0, 92.0);

/// Inward-curving fillets at the top corners that melt the notch into the
/// screen edge, like a MacBook notch; plain CSS radii can't draw concave shapes.
const EAR: f64 = 8.0;
/// Bottom corners grow with the notch so the big card doesn't look boxy.
const RADIUS_COLLAPSED: f64 = 12.0;
const RADIUS_EXPANDED: f64 = 28.0;

/// The window is fixed at the largest size; only the notch inside changes,
/// because resizing an override-redirect GTK window races with GTK itself.
pub const WIN_W: i32 = EXPANDED_LYRICS.0 as i32 + 2 * EAR as i32;
pub const WIN_H: i32 = EXPANDED_LYRICS.1 as i32;

const ANIM: Duration = Duration::from_millis(250);
/// Grace period before collapsing, so brushing past the edge doesn't flicker.
const COLLAPSE_DELAY: Duration = Duration::from_millis(300);
const CONTENT_FADE_MS: u32 = 150;
/// How long a peek stays before folding back.
const PEEK_HOLD: Duration = Duration::from_millis(3500);
/// A peek that arrives while the notch is open waits for it to close; after
/// this long it's old news and is dropped.
const PEEK_STALE: Duration = Duration::from_secs(6);

/// Notch bounds in window coordinates (x, y, w, h), shared with the X event
/// thread, which turns pointer positions into hover changes.
pub type HitRect = Arc<Mutex<(i32, i32, i32, i32)>>;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Collapsed,
    Expanded,
    Peek,
}

struct Anim {
    from: (f64, f64),
    to: (f64, f64),
    start_us: i64,
}

pub struct Island {
    pub root: gtk::Overlay,
    notch: DrawingArea,
    stack: Stack,
    collapsed: Collapsed,
    music: Rc<MusicCard>,
    peek: PeekCard,
    peek_timer: RefCell<Option<glib::SourceId>>,
    queued_peek: RefCell<Option<(Peek, std::time::Instant)>>,
    state: Cell<State>,
    has_music: Cell<bool>,
    size: Rc<Cell<(f64, f64)>>,
    anim: RefCell<Option<Anim>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    collapse_timer: RefCell<Option<glib::SourceId>>,
    art_url: RefCell<Option<String>>,
    hit: HitRect,
    lyrics_mode: Cell<bool>,
    /// Pinned: stays open when the pointer leaves, e.g. to read lyrics while
    /// working.
    pinned: Cell<bool>,
    hovered: Cell<bool>,
    /// Song the lyrics panel should show; lookups only happen in lyrics mode,
    /// so nothing about the music leaves the machine unless asked for.
    lyrics_key: RefCell<Option<lyrics::Key>>,
    lyrics_loaded: RefCell<Option<lyrics::Key>>,
    lyrics_requests: async_channel::Sender<lyrics::Request>,
    length: Cell<Option<Duration>>,
}

impl Island {
    pub fn new(
        hit: HitRect,
        commands: async_channel::Sender<Command>,
        lyrics_requests: async_channel::Sender<lyrics::Request>,
    ) -> Rc<Self> {
        Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let weak = weak.clone();
            let w = weak.clone();
            let on_lyrics = move || {
                if let Some(island) = w.upgrade() {
                    island.toggle_lyrics();
                }
            };
            let on_pin = move || {
                if let Some(island) = weak.upgrade() {
                    island.toggle_pin();
                }
            };
            Self::build(hit, commands, lyrics_requests, on_lyrics, on_pin)
        })
    }

    fn build(
        hit: HitRect,
        commands: async_channel::Sender<Command>,
        lyrics_requests: async_channel::Sender<lyrics::Request>,
        on_lyrics: impl Fn() + 'static,
        on_pin: impl Fn() + 'static,
    ) -> Self {
        let size = Rc::new(Cell::new(COLLAPSED));
        let notch = DrawingArea::builder()
            .content_width(WIN_W)
            .content_height(WIN_H)
            .build();
        let s = size.clone();
        notch.set_draw_func(move |_, cr, w, _| draw(cr, f64::from(w), s.get()));

        let collapsed = Collapsed::new();
        let music = MusicCard::new(commands, on_lyrics, on_pin);
        let idle = Idle::new();

        // Pages size to their own content, so the overlay never forces the
        // notch area bigger than the visible card.
        let stack = Stack::builder()
            .hhomogeneous(false)
            .vhomogeneous(false)
            .transition_duration(CONTENT_FADE_MS)
            .halign(Align::Center)
            .valign(Align::Start)
            .build();
        stack.add_named(&collapsed.root, Some("collapsed"));
        stack.add_named(&music.root, Some("music"));
        stack.add_named(&idle.root, Some("idle"));
        let peek = PeekCard::new();
        stack.add_named(&peek.root, Some("peek"));
        stack.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("none"));
        stack.set_visible_child_name("collapsed");

        let root = gtk::Overlay::builder().child(&notch).build();
        root.add_overlay(&stack);

        let island = Self {
            root,
            notch,
            stack,
            collapsed,
            music,
            state: Cell::new(State::Collapsed),
            has_music: Cell::new(false),
            size,
            anim: RefCell::new(None),
            tick: RefCell::new(None),
            collapse_timer: RefCell::new(None),
            peek,
            peek_timer: RefCell::new(None),
            queued_peek: RefCell::new(None),
            art_url: RefCell::new(None),
            hit,
            lyrics_mode: Cell::new(false),
            pinned: Cell::new(false),
            hovered: Cell::new(false),
            lyrics_key: RefCell::new(None),
            lyrics_loaded: RefCell::new(None),
            lyrics_requests,
            length: Cell::new(None),
        };

        // The input region needs a realized surface, which exists by map.
        let hit = island.hit.clone();
        island
            .root
            .connect_map(move |root| set_bounds(root, &hit, COLLAPSED));
        island
    }

    pub fn on_hover(self: &Rc<Self>, inside: bool) {
        self.hovered.set(inside);
        if let Some(id) = self.collapse_timer.take() {
            id.remove();
        }
        if inside {
            // Hovering a peek opens the full card, like hovering the notch.
            if self.state.get() != State::Expanded {
                self.cancel_peek_timer();
                self.expand();
            }
            return;
        }
        if self.pinned.get() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(COLLAPSE_DELAY, move || {
            if let Some(island) = weak.upgrade() {
                island.collapse_timer.take();
                island.collapse();
            }
        });
        self.collapse_timer.replace(Some(id));
    }

    pub fn on_music(self: &Rc<Self>, update: Update) {
        match update {
            Update::Track(Some(np)) => {
                if self.art_url.borrow().as_ref() != np.art_url.as_ref() {
                    self.art_url.replace(np.art_url.clone());
                    self.set_art(None);
                }
                self.music.update(&np);
                self.collapsed.set_track(Some(&np));
                self.length.set(np.length);
                self.lyrics_key.replace(Some(lyrics::Key {
                    title: np.title.clone(),
                    artist: np.artist.clone(),
                }));
                self.request_lyrics();
                self.set_has_music(true);
            }
            Update::Track(None) => {
                self.art_url.replace(None);
                self.set_art(None);
                self.collapsed.set_track(None);
                self.lyrics_key.replace(None);
                self.set_has_music(false);
                if self.pinned.get() {
                    self.toggle_pin();
                }
            }
            Update::Art { url, bytes } => {
                // Ignore covers that arrive after the track already changed.
                if self.art_url.borrow().as_deref() != Some(url.as_str()) {
                    return;
                }
                match gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) {
                    Ok(tex) => self.set_art(Some(&tex)),
                    Err(e) => tracing::debug!("cover art not decodable: {e}"),
                }
            }
        }
    }

    pub fn on_lyrics(&self, update: lyrics::Update) {
        if self.lyrics_key.borrow().as_ref() != Some(&update.key) {
            return; // stale: the song changed while we were looking
        }
        self.music.set_lyrics(match update.lines {
            Some(lines) => Lyrics::Lines(lines),
            None => Lyrics::Missing,
        });
    }

    fn toggle_lyrics(self: &Rc<Self>) {
        let on = !self.lyrics_mode.get();
        self.lyrics_mode.set(on);
        self.music.set_lyrics_mode(on);
        self.request_lyrics();
        if self.state.get() == State::Expanded {
            self.expand(); // morph to the new width
        }
    }

    fn toggle_pin(self: &Rc<Self>) {
        let pinned = !self.pinned.get();
        self.pinned.set(pinned);
        self.music.set_pinned(pinned);
        if !pinned && !self.hovered.get() {
            self.on_hover(false); // released while away: close as usual
        }
    }

    /// Asks for the current song's lyrics once, and only in lyrics mode.
    fn request_lyrics(&self) {
        let key = self.lyrics_key.borrow().clone();
        if !self.lyrics_mode.get() || key.is_none() || *self.lyrics_loaded.borrow() == key {
            return;
        }
        let Some(key) = key else { return };
        self.lyrics_loaded.replace(Some(key.clone()));
        self.music.set_lyrics(Lyrics::Loading);
        let _ = self.lyrics_requests.try_send(lyrics::Request {
            key,
            length: self.length.get(),
        });
    }

    fn set_art(&self, art: Option<&gdk::Texture>) {
        self.music.set_art(art);
    }

    fn set_has_music(self: &Rc<Self>, has: bool) {
        if self.has_music.replace(has) == has {
            return;
        }
        // Already open: morph straight into the other card.
        if self.state.get() == State::Expanded {
            self.expand();
        }
    }

    fn expanded_page(&self) -> (&'static str, (f64, f64)) {
        if self.has_music.get() && self.lyrics_mode.get() {
            ("music", EXPANDED_LYRICS)
        } else if self.has_music.get() {
            ("music", EXPANDED_MUSIC)
        } else {
            ("idle", EXPANDED_IDLE)
        }
    }

    fn expand(self: &Rc<Self>) {
        self.state.set(State::Expanded);
        let (_, target) = self.expanded_page();
        self.set_bounds(target);
        self.show_page("none", false);
        self.animate_to(target);
    }

    /// Shows a short notification, then folds back. Never interrupts an
    /// open card: it waits until the notch closes.
    pub fn on_peek(self: &Rc<Self>, peek: Peek) {
        if self.state.get() == State::Expanded {
            self.queued_peek
                .replace(Some((peek, std::time::Instant::now())));
            return;
        }
        self.peek.show(&peek);
        self.cancel_peek_timer();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(PEEK_HOLD, move || {
            if let Some(island) = weak.upgrade() {
                island.peek_timer.take();
                if island.state.get() == State::Peek {
                    island.collapse();
                }
            }
        });
        self.peek_timer.replace(Some(id));
        if self.state.replace(State::Peek) == State::Peek {
            self.show_page("peek", false); // already open: just swap content
            return;
        }
        self.set_bounds(PEEK);
        self.show_page("none", false);
        self.animate_to(PEEK);
    }

    fn cancel_peek_timer(&self) {
        if let Some(id) = self.peek_timer.take() {
            id.remove();
        }
    }

    fn collapse(self: &Rc<Self>) {
        if self.state.replace(State::Collapsed) == State::Collapsed {
            return;
        }
        self.music.set_active(false);
        self.set_bounds(COLLAPSED);
        self.show_page("none", false);
        self.animate_to(COLLAPSED);
    }

    /// Content appears only once the notch has reached its size; growing the
    /// outline around fixed-size content would spill it outside the shape.
    fn on_settled(self: &Rc<Self>) {
        match self.state.get() {
            State::Collapsed => {
                self.show_page("collapsed", true);
                let queued = self.queued_peek.take();
                if let Some((peek, at)) = queued {
                    if at.elapsed() < PEEK_STALE {
                        self.on_peek(peek);
                    }
                }
            }
            State::Peek => self.show_page("peek", true),
            State::Expanded => {
                let (page, _) = self.expanded_page();
                self.show_page(page, true);
                self.music.set_active(page == "music");
            }
        }
    }

    fn show_page(&self, name: &str, fade: bool) {
        let transition = if fade {
            StackTransitionType::Crossfade
        } else {
            StackTransitionType::None
        };
        self.stack.set_visible_child_full(name, transition);
    }

    fn animate_to(self: &Rc<Self>, to: (f64, f64)) {
        let Some(clock) = self.notch.frame_clock() else {
            // Not on screen yet: jump.
            self.size.set(to);
            self.notch.queue_draw();
            self.on_settled();
            return;
        };
        self.anim.replace(Some(Anim {
            from: self.size.get(),
            to,
            start_us: clock.frame_time(),
        }));
        if self.tick.borrow().is_some() {
            return; // the running tick picks up the new target
        }

        // A tick callback runs only while it's installed, so idle = no frames.
        let weak = Rc::downgrade(self);
        let id = self.notch.add_tick_callback(move |_, clock| {
            let Some(island) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            island.step(clock.frame_time())
        });
        self.tick.replace(Some(id));
    }

    fn step(self: &Rc<Self>, now_us: i64) -> glib::ControlFlow {
        let (size, done) = match self.anim.borrow().as_ref() {
            Some(a) => {
                let t = ((now_us - a.start_us) as f64 / ANIM.as_micros() as f64).clamp(0.0, 1.0);
                let e = ease_out(t);
                let lerp = |x: f64, y: f64| x + (y - x) * e;
                ((lerp(a.from.0, a.to.0), lerp(a.from.1, a.to.1)), t >= 1.0)
            }
            None => (self.size.get(), true),
        };
        self.size.set(size);
        self.notch.queue_draw();
        if !done {
            return glib::ControlFlow::Continue;
        }
        self.anim.replace(None);
        self.tick.take();
        self.on_settled();
        glib::ControlFlow::Break
    }

    fn set_bounds(&self, size: (f64, f64)) {
        set_bounds(&self.root, &self.hit, size);
    }
}

/// Clicks outside the notch must fall through to whatever is below, and
/// hover is judged against the same rectangle.
fn set_bounds(root: &impl IsA<gtk::Widget>, hit: &HitRect, (w, h): (f64, f64)) {
    let rect = ((WIN_W - w as i32) / 2, 0, w as i32, h as i32);
    if let Ok(mut r) = hit.lock() {
        *r = rect;
    }
    let Some(surface) = root.native().and_then(|n| n.surface()) else {
        return;
    };
    let region =
        cairo::Region::create_rectangle(&cairo::RectangleInt::new(rect.0, rect.1, rect.2, rect.3));
    surface.set_input_region(Some(&region));
}

/// Quartic ease-out: fast start, soft landing, never overshoots.
fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(4)
}

fn draw(cr: &cairo::Context, win_w: f64, (body_w, h): (f64, f64)) {
    let progress = ((h - COLLAPSED.1) / (EXPANDED_MUSIC.1 - COLLAPSED.1)).clamp(0.0, 1.0);
    let r = (RADIUS_COLLAPSED + (RADIUS_EXPANDED - RADIUS_COLLAPSED) * progress).min(h / 2.0);
    let w = body_w + 2.0 * EAR;
    let x0 = ((win_w - w) / 2.0).round();

    cr.translate(x0, 0.0);
    cr.move_to(0.0, 0.0);
    // Left ear: concave curve from the screen edge down into the body.
    cr.curve_to(EAR, 0.0, EAR, 0.0, EAR, EAR);
    cr.line_to(EAR, h - r);
    cr.arc_negative(EAR + r, h - r, r, 2.0 * FRAC_PI_2, FRAC_PI_2);
    cr.line_to(w - EAR - r, h);
    cr.arc_negative(w - EAR - r, h - r, r, FRAC_PI_2, 0.0);
    cr.line_to(w - EAR, EAR);
    // Right ear, mirrored.
    cr.curve_to(w - EAR, 0.0, w - EAR, 0.0, w, 0.0);
    cr.close_path();

    let (r, g, b) = crate::palette::NOTCH;
    cr.set_source_rgb(r, g, b);
    if let Err(e) = cr.fill() {
        tracing::warn!("notch fill failed: {e}");
    }
}
