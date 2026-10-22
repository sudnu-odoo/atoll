//! The notch: its outline, collapsed/expanded state, animation, and which
//! part of the window accepts input.

use std::cell::{Cell, RefCell};
use std::f64::consts::FRAC_PI_2;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, Align, DrawingArea, Stack, StackTransitionType};

use crate::cards::claude::ClaudeCard;
use crate::cards::clock::{Collapsed, Idle};
use crate::cards::control_button;
use crate::cards::music::{Lyrics, MusicCard};
use crate::cards::peek::{Peek, PeekCard};
use crate::cards::water::WaterCard;
use crate::config::Features;
use crate::services::mpris::{Command, Update};
use crate::services::{claude, lyrics};
use crate::water;

/// Body sizes (without the ears) for each state.
pub const COLLAPSED: (f64, f64) = (200.0, 32.0);
// Expanded cards keep 40px each side (see style.css) for the page arrows.
// Cards end ~10px above the bottom edge, leaving a strip for the page dots.
pub const EXPANDED_MUSIC: (f64, f64) = (544.0, 206.0);
pub const EXPANDED_LYRICS: (f64, f64) = (844.0, 206.0);
pub const EXPANDED_WATER: (f64, f64) = (544.0, 206.0);
pub const EXPANDED_IDLE: (f64, f64) = (340.0, 128.0);
/// Claude prompts: fixed width, height grows with the content up to
/// CLAUDE_MAX_H (long questions, many options).
pub const EXPANDED_CLAUDE: (f64, f64) = (600.0, 206.0);
pub const CLAUDE_MAX_H: f64 = 360.0;
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
pub const WIN_H: i32 = CLAUDE_MAX_H as i32;

/// Height of the GNOME top bar: the notch area above this line can't be
/// clicked (GNOME Shell takes the input), only looked at.
const BAR_H: i32 = 32;

const ANIM: Duration = Duration::from_millis(250);
/// Grace period before collapsing, so brushing past the edge doesn't flicker.
const COLLAPSE_DELAY: Duration = Duration::from_millis(300);
const CONTENT_FADE_MS: u32 = 150;
/// How long a peek stays before folding back.
const PEEK_HOLD: Duration = Duration::from_millis(3500);
/// A peek that arrives while the notch is open waits for it to close; after
/// this long it's old news and is dropped.
const PEEK_STALE: Duration = Duration::from_secs(6);

/// Pointer events from the X thread, in island-window coordinates.
pub enum Pointer {
    Hover(bool),
    /// A primary click on the notch, seen even where GTK can't see it (over
    /// the GNOME bar).
    Click(i32, i32),
}

/// Everything an island needs from the rest of the app: where to send user
/// actions, and which features are switched on.
#[derive(Clone)]
pub struct Deps {
    pub commands: async_channel::Sender<Command>,
    pub lyrics_requests: async_channel::Sender<lyrics::Request>,
    pub water_commands: async_channel::Sender<water::Command>,
    pub open_settings: async_channel::Sender<()>,
    pub claude_answers: async_channel::Sender<claude::Reply>,
    pub features: Features,
}

/// Notch bounds in window coordinates (x, y, w, h), shared with the X event
/// thread, which turns pointer positions into hover changes.
pub type HitRect = Arc<Mutex<(i32, i32, i32, i32)>>;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Collapsed,
    Expanded,
    Peek,
    /// A Claude Code prompt is waiting: open on its own, and stays open
    /// until answered or timed out, whatever the pointer does.
    Claude,
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
    collapsed: Rc<Collapsed>,
    music: Rc<MusicCard>,
    water: WaterCard,
    claude: Rc<ClaudeCard>,
    features: Features,
    /// Index into `pages()` of the expanded card being shown.
    page: Cell<usize>,
    prev: gtk::Button,
    next: gtk::Button,
    dots: gtk::Box,
    gear: gtk::Button,
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
    pub fn new(hit: HitRect, deps: Deps) -> Rc<Self> {
        let island = Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let w = weak.clone();
            let on_lyrics = move || {
                if let Some(island) = w.upgrade() {
                    island.toggle_lyrics();
                }
            };
            let w = weak.clone();
            let on_pin = move || {
                if let Some(island) = w.upgrade() {
                    island.toggle_pin();
                }
            };
            Self::build(hit, deps, on_lyrics, on_pin)
        });
        island.connect_pager();
        // Next question in a multi-question prompt: refit the notch.
        let weak = Rc::downgrade(&island);
        island.claude.set_on_layout(move || {
            if let Some(i) = weak.upgrade() {
                if i.state.get() == State::Claude {
                    i.fit_claude();
                }
            }
        });
        island
    }

    fn build(
        hit: HitRect,
        deps: Deps,
        on_lyrics: impl Fn() + 'static,
        on_pin: impl Fn() + 'static,
    ) -> Self {
        let Deps {
            commands,
            lyrics_requests,
            water_commands,
            open_settings,
            claude_answers,
            features,
        } = deps;
        let size = Rc::new(Cell::new(COLLAPSED));
        let notch = DrawingArea::builder()
            .content_width(WIN_W)
            .content_height(WIN_H)
            .build();
        let s = size.clone();
        notch.set_draw_func(move |_, cr, w, _| draw(cr, f64::from(w), s.get()));

        let collapsed = Collapsed::new(features.eyes);
        let music = MusicCard::new(commands, on_lyrics, on_pin);
        music.set_lyrics_available(features.lyrics);
        let idle = Idle::new();
        let water = WaterCard::new(water_commands);
        let claude = ClaudeCard::new(claude_answers);

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
        stack.add_named(&water.root, Some("water"));
        stack.add_named(&claude.root, Some("claude"));
        let peek = PeekCard::new();
        stack.add_named(&peek.root, Some("peek"));
        stack.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("none"));
        stack.set_visible_child_name("collapsed");

        // Page arrows and dots float over the card. They're separate overlay
        // children (not one full-size layer) so they never block clicks on
        // the card itself.
        let prev = control_button(&["pager"], Some("atoll-left-symbolic"));
        let next = control_button(&["pager"], Some("atoll-right-symbolic"));
        let dots = gtk::Box::builder()
            .css_classes(["dots"])
            .spacing(5)
            .halign(Align::Center)
            .valign(Align::Start)
            .can_target(false)
            .build();
        for (w, h) in [(&prev, Align::Start), (&next, Align::End)] {
            w.set_halign(h);
            w.set_valign(Align::Start);
            w.set_visible(false);
        }
        dots.set_visible(false);
        let gear = control_button(&["pager", "gear"], Some("atoll-settings-symbolic"));
        gear.set_tooltip_text(Some("Atoll settings"));
        gear.set_halign(Align::End);
        gear.set_valign(Align::Start);
        gear.set_visible(false);
        gear.connect_clicked(move |_| {
            let _ = open_settings.try_send(());
        });

        let root = gtk::Overlay::builder().child(&notch).build();
        root.add_overlay(&stack);
        root.add_overlay(&prev);
        root.add_overlay(&next);
        root.add_overlay(&dots);
        root.add_overlay(&gear);

        let island = Self {
            root,
            notch,
            stack,
            collapsed,
            music,
            water,
            claude,
            features,
            page: Cell::new(0),
            prev,
            next,
            dots,
            gear,
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

    pub fn on_pointer(self: &Rc<Self>, event: Pointer) {
        match event {
            Pointer::Hover(inside) => self.on_hover(inside),
            Pointer::Click(x, y) => self.on_click(x, f64::from(y)),
        }
    }

    /// Handles clicks GTK may not have received: only the gear lives in the
    /// bar area. On monitors without a bar GTK sees the click as well, and
    /// opening settings twice just raises the window.
    fn on_click(&self, x: i32, y: f64) {
        if self.state.get() != State::Expanded || !self.gear.is_visible() {
            return;
        }
        let Some(b) = self.gear.compute_bounds(&self.root) else {
            return;
        };
        let x = f64::from(x) as f32;
        if b.contains_point(&gtk::graphene::Point::new(x, y as f32)) {
            self.gear.emit_clicked();
        }
    }

    fn on_hover(self: &Rc<Self>, inside: bool) {
        self.hovered.set(inside);
        if self.state.get() == State::Claude {
            return; // a prompt holds the notch open regardless
        }
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

    pub fn on_water(&self, view: &water::View) {
        self.water.update(view);
    }

    pub fn on_music(self: &Rc<Self>, update: Update) {
        if !self.features.music {
            return; // music switched off: behave as if nothing plays
        }
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

    /// Expanded cards in order: music (or the clock card when nothing
    /// plays), then water.
    fn pages(&self) -> Vec<(&'static str, (f64, f64))> {
        let mut pages = Vec::new();
        if self.has_music.get() && self.lyrics_mode.get() {
            pages.push(("music", EXPANDED_LYRICS));
        } else if self.has_music.get() {
            pages.push(("music", EXPANDED_MUSIC));
        } else {
            pages.push(("idle", EXPANDED_IDLE));
        }
        if self.features.water {
            pages.push(("water", EXPANDED_WATER));
        }
        pages
    }

    fn expanded_page(&self) -> (&'static str, (f64, f64)) {
        let pages = self.pages();
        let i = self.page.get().min(pages.len() - 1);
        pages[i]
    }

    /// Steps to the next/previous card, wrapping around.
    fn go(self: &Rc<Self>, delta: isize) {
        let n = self.pages().len();
        if n < 2 || self.state.get() != State::Expanded || self.tick.borrow().is_some() {
            return;
        }
        let i = (self.page.get().min(n - 1) as isize + delta).rem_euclid(n as isize);
        self.page.set(i as usize);
        self.expand();
    }

    fn connect_pager(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.prev.connect_clicked(move |_| {
            if let Some(i) = weak.upgrade() {
                i.go(-1);
            }
        });
        let weak = Rc::downgrade(self);
        self.next.connect_clicked(move |_| {
            if let Some(i) = weak.upgrade() {
                i.go(1);
            }
        });
        // Scrolling over the card flips pages too, like a carousel.
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::DISCRETE,
        );
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |_, dx, dy| {
            let delta = if dx.abs() > dy.abs() { dx } else { dy };
            if let Some(i) = weak.upgrade() {
                if delta != 0.0 {
                    i.go(if delta > 0.0 { 1 } else { -1 });
                }
            }
            glib::Propagation::Stop
        });
        self.stack.add_controller(scroll);
    }

    /// Places the arrows and dots on the settled card, or hides them.
    fn show_pager(&self, visible: bool) {
        // The gear sits in the top-right corner of every expanded card. That
        // is over the GNOME bar, so its clicks arrive via `on_click`.
        self.gear.set_visible(visible);
        if visible {
            let w = self.expanded_page().1 .0;
            // Same column as the › arrow below it.
            self.gear.set_margin_end((WIN_W - w as i32) / 2 + 8);
            // Centered on the cards' top row (16px padding + 26px row).
            self.gear.set_margin_top(16);
        }
        let pages = self.pages();
        let show = visible && pages.len() > 1;
        self.prev.set_visible(show);
        self.next.set_visible(show);
        self.dots.set_visible(show);
        if !show {
            return;
        }
        let (w, h) = self.expanded_page().1;
        let inset = (WIN_W - w as i32) / 2 + 8;
        // Vertically centered on the part of the card below the GNOME bar,
        // where clicks reach us.
        let top = (BAR_H + h as i32) / 2 - 14;
        self.prev.set_margin_start(inset);
        self.prev.set_margin_top(top);
        self.next.set_margin_end(inset);
        self.next.set_margin_top(top);
        self.dots.set_margin_top(h as i32 - 11);

        while let Some(child) = self.dots.first_child() {
            self.dots.remove(&child);
        }
        let current = self.page.get().min(pages.len() - 1);
        for i in 0..pages.len() {
            let dot = gtk::Box::builder()
                .css_classes(if i == current {
                    ["dot", "current"]
                } else {
                    ["dot", "other"]
                })
                .build();
            self.dots.append(&dot);
        }
    }

    fn expand(self: &Rc<Self>) {
        self.state.set(State::Expanded);
        self.show_pager(false);
        let (_, target) = self.expanded_page();
        self.set_bounds(target);
        self.show_page("none", false);
        self.animate_to(target);
    }

    /// Shows a short notification, then folds back. Never interrupts an
    /// open card: it waits until the notch closes.
    /// Shows a Claude prompt (`Some`) or clears it (`None`: answered,
    /// timed out, or answered on another monitor).
    pub fn on_claude(self: &Rc<Self>, request: Option<&claude::Request>) {
        if let Some(r) = request {
            if let Some(id) = self.collapse_timer.take() {
                id.remove();
            }
            self.cancel_peek_timer();
            self.music.set_active(false);
            self.show_pager(false);
            self.state.set(State::Claude);
            self.claude.show(r);
            self.fit_claude();
            return;
        }
        if self.state.get() != State::Claude {
            return;
        }
        self.claude.stop();
        if self.hovered.get() {
            self.expand(); // still under the pointer: back to the normal card
        } else {
            self.state.set(State::Expanded);
            self.collapse();
        }
    }

    /// Sizes the notch to the prompt's content.
    fn fit_claude(self: &Rc<Self>) {
        let h =
            f64::from(self.claude.natural_height()).clamp(EXPANDED_CLAUDE.1 * 0.75, CLAUDE_MAX_H);
        let size = (EXPANDED_CLAUDE.0, h);
        self.set_bounds(size);
        self.show_page("none", false);
        self.animate_to(size);
    }

    pub fn on_peek(self: &Rc<Self>, peek: Peek) {
        if matches!(self.state.get(), State::Expanded | State::Claude) {
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
        self.show_pager(false);
        // Next hover starts from the first card again.
        self.page.set(0);
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
            State::Claude => self.show_page("claude", true),
            State::Expanded => {
                let (page, _) = self.expanded_page();
                self.show_page(page, true);
                self.music.set_active(page == "music");
                self.show_pager(true);
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
