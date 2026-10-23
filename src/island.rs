//! The notch: its outline, collapsed/expanded state, animation, and which
//! part of the window accepts input.

use std::cell::{Cell, RefCell};
use std::f64::consts::FRAC_PI_2;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, Align, DrawingArea, Stack, StackTransitionType};

use crate::cards::calendar::CalendarCard;
use crate::cards::claude::ClaudeCard;
use crate::cards::clock::{Collapsed, Idle};
use crate::cards::control_button;
use crate::cards::controls::ControlsCard;
use crate::cards::music::{Lyrics, MusicCard};
use crate::cards::notifications::NotificationsCard;
use crate::cards::peek::{Kind, Peek, PeekCard};
use crate::cards::shelf::ShelfCard;
use crate::cards::todo::{TodoCard, Typing};
use crate::cards::water::WaterCard;
use crate::config::Features;
use crate::services::mpris::{Command, Update};
use crate::services::{claude, lyrics};
use crate::water;

/// Body sizes (without the ears) for each state.
/// Wide enough to cover GNOME's whole date and time in the bar.
pub const COLLAPSED: (f64, f64) = (250.0, 32.0);
// Expanded cards keep a 52px margin each side (see style.css) for the page
// arrows and the settings button, with room to spare around them.
// Cards end ~10px above the bottom edge, leaving a strip for the page dots.
pub const EXPANDED_MUSIC: (f64, f64) = (568.0, 206.0);
pub const EXPANDED_LYRICS: (f64, f64) = (868.0, 206.0);
pub const EXPANDED_WATER: (f64, f64) = (568.0, 206.0);
pub const EXPANDED_IDLE: (f64, f64) = (364.0, 128.0);
pub const EXPANDED_TODO: (f64, f64) = (568.0, 206.0);
pub const EXPANDED_CALENDAR: (f64, f64) = (568.0, 252.0);
pub const EXPANDED_SHELF: (f64, f64) = (568.0, 206.0);
pub const EXPANDED_CONTROLS: (f64, f64) = (568.0, 206.0);
pub const EXPANDED_NOTIFICATIONS: (f64, f64) = (568.0, 206.0);
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
    /// A fullscreen window covers this island's monitor.
    Fullscreen(bool),
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
    pub todo_commands: async_channel::Sender<crate::todo::Command>,
    pub shelf_commands: async_channel::Sender<crate::shelf::Command>,
    pub controls_commands: async_channel::Sender<crate::services::controls::Command>,
    pub clear_notifications: async_channel::Sender<()>,
    /// Ask the calendar service for a month (answered via `on_month`).
    pub calendar_months: async_channel::Sender<(i32, u32)>,
    /// The lyrics translate button: on/off, remembered by the app.
    pub set_translate: async_channel::Sender<bool>,
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
    todo: Rc<TodoCard>,
    calendar: Rc<CalendarCard>,
    shelf: Rc<ShelfCard>,
    controls: Rc<ControlsCard>,
    notifications: Rc<NotificationsCard>,
    shelf_commands: async_channel::Sender<crate::shelf::Command>,
    /// A file is being dragged over the notch or out of the shelf: stay open.
    dragging: Cell<bool>,
    claude: Rc<ClaudeCard>,
    features: Features,
    /// Index into `pages()` of the expanded card being shown.
    page: Cell<usize>,
    prev: gtk::Button,
    next: gtk::Button,
    dots: gtk::Box,
    gear: gtk::Button,
    /// Under the › arrow, only beside open lyrics.
    translate_button: gtk::Button,
    translate: Cell<bool>,
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
    fullscreen: Cell<bool>,
    /// The card the current peek belongs to, opened if the peek is hovered.
    peek_page: Cell<Option<&'static str>>,
    /// The user is typing in the notch: it holds keyboard focus (taken from
    /// this window) and stays open until typing ends.
    typing_from: Cell<Option<Option<u32>>>,
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
            let w = weak.clone();
            let on_typing = move |t| {
                if let Some(island) = w.upgrade() {
                    island.on_typing(t);
                }
            };
            let w = weak.clone();
            let on_drag = move |active| {
                if let Some(island) = w.upgrade() {
                    island.set_dragging(active);
                }
            };
            let w = weak.clone();
            let on_controls_typing = move |t| {
                if let Some(island) = w.upgrade() {
                    island.on_typing(t);
                }
            };
            Self::build(
                hit,
                deps,
                on_lyrics,
                on_pin,
                on_typing,
                on_drag,
                on_controls_typing,
            )
        });
        island.connect_pager();
        island.accept_drops(shelf_commands_for_drop(&island));
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
        on_typing: impl Fn(Typing) + 'static,
        on_drag: impl Fn(bool) + 'static,
        on_controls_typing: impl Fn(Typing) + 'static,
    ) -> Self {
        let Deps {
            commands,
            lyrics_requests,
            water_commands,
            open_settings,
            claude_answers,
            todo_commands,
            calendar_months,
            shelf_commands,
            controls_commands,
            clear_notifications,
            set_translate,
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
        let todo = TodoCard::new(todo_commands, on_typing);

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
        stack.add_named(&todo.root, Some("todo"));
        let shelf = ShelfCard::new(shelf_commands.clone(), on_drag);
        stack.add_named(&shelf.root, Some("shelf"));
        let controls = ControlsCard::new(controls_commands, on_controls_typing);
        stack.add_named(&controls.root, Some("controls"));
        let notifications = NotificationsCard::new(move || {
            let _ = clear_notifications.try_send(());
        });
        stack.add_named(&notifications.root, Some("notifications"));
        let calendar = CalendarCard::new(move |y, m| {
            let _ = calendar_months.try_send((y, m));
        });
        stack.add_named(&calendar.root, Some("calendar"));
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
        let translate_button =
            control_button(&["pager", "translate"], Some("atoll-translate-symbolic"));
        translate_button.set_tooltip_text(Some("Translate to English"));
        translate_button.set_halign(Align::End);
        translate_button.set_valign(Align::Start);
        translate_button.set_visible(false);
        if features.translate_lyrics {
            translate_button.add_css_class("on");
        }
        translate_button.connect_clicked(move |b| {
            let _ = set_translate.try_send(!b.has_css_class("on"));
        });
        root.add_overlay(&translate_button);

        let island = Self {
            root,
            notch,
            stack,
            collapsed,
            music,
            water,
            todo,
            calendar,
            shelf,
            controls,
            notifications,
            shelf_commands,
            dragging: Cell::new(false),
            claude,
            features,
            page: Cell::new(0),
            prev,
            next,
            dots,
            gear,
            translate: Cell::new(translate_button.has_css_class("on")),
            translate_button,
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
            fullscreen: Cell::new(false),
            peek_page: Cell::new(None),
            typing_from: Cell::new(None),
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

    /// Developer aid (`ATOLL_SHOW=<card>`): opens a card at startup and
    /// keeps it open, so layouts can be checked with a screenshot instead
    /// of hovering.
    pub fn show_for_debugging(self: &Rc<Self>, page: &str) {
        let page = match page {
            "todo-add" | "todo-edit" => {
                self.todo.show_form_for_debugging(page == "todo-edit");
                "todo"
            }
            "controls-wifi" => {
                let c = self.controls.clone();
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    c.show_menu_for_debugging();
                });
                "controls"
            }
            other => other,
        };
        let Some(i) = self.pages().iter().position(|(name, _)| *name == page) else {
            tracing::warn!("ATOLL_SHOW: no card named {page}");
            return;
        };
        self.page.set(i);
        self.pinned.set(true);
        self.expand();
    }

    pub fn on_pointer(self: &Rc<Self>, event: Pointer) {
        match event {
            Pointer::Hover(inside) => self.on_hover(inside),
            Pointer::Click(x, y) => self.on_click(x, f64::from(y)),
            Pointer::Fullscreen(on) => self.set_fullscreen(on),
        }
    }

    /// Steps aside for fullscreen windows: hidden, and no input region, so
    /// nothing of the island can be seen or clicked. A Claude prompt still
    /// shows: it's waiting on the user and would otherwise time out unseen.
    fn set_fullscreen(self: &Rc<Self>, on: bool) {
        self.fullscreen.set(on);
        let hide = on && self.state.get() != State::Claude;
        self.root.set_visible(!hide);
        if hide {
            if let Some(surface) = self.root.native().and_then(|n| n.surface()) {
                surface.set_input_region(Some(&cairo::Region::create()));
            }
        } else {
            self.set_bounds(self.size.get());
        }
    }

    /// Clicks over the GNOME bar (sent only for the primary monitor's
    /// island) never reach GTK: find the button under the pointer and click
    /// it ourselves. Below the bar GTK handles clicks normally.
    fn on_click(&self, x: i32, y: f64) {
        if y >= f64::from(BAR_H) || self.state.get() != State::Expanded {
            return;
        }
        let mut widget = self.root.pick(f64::from(x), y, gtk::PickFlags::DEFAULT);
        while let Some(w) = widget {
            if let Some(button) = w.downcast_ref::<gtk::Button>() {
                if button.is_sensitive() {
                    button.emit_clicked();
                }
                return;
            }
            widget = w.parent();
        }
    }

    fn on_hover(self: &Rc<Self>, inside: bool) {
        self.hovered.set(inside);
        if self.state.get() == State::Claude {
            return; // a prompt holds the notch open regardless
        }
        if self.fullscreen.get() {
            return; // hidden: the pointer is just near the screen edge
        }
        if !inside && (self.typing_from.get().is_some() || self.dragging.get()) {
            return; // mid-typing or mid-drag: stay open
        }
        if let Some(id) = self.collapse_timer.take() {
            id.remove();
        }
        if inside {
            // Hovering a peek opens the card it belongs to (a notification
            // opens the notifications, a meeting the calendar...), so it can
            // be acted on right away; otherwise the first card.
            if self.state.get() != State::Expanded {
                self.cancel_peek_timer();
                let related = self
                    .peek_page
                    .take()
                    .filter(|_| self.state.get() == State::Peek);
                match related {
                    Some(page) if self.pages().iter().any(|(p, _)| *p == page) => {
                        self.show_named_page(page);
                    }
                    _ => self.expand(),
                }
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

    fn on_typing(self: &Rc<Self>, typing: Typing) {
        match typing {
            Typing::Started => {
                if self.typing_from.get().is_none() {
                    self.typing_from
                        .set(Some(crate::window::take_focus(&self.root)));
                }
            }
            Typing::Ended { restore_focus } => {
                let Some(previous) = self.typing_from.take() else {
                    return;
                };
                if let (true, Some(prev)) = (restore_focus, previous) {
                    crate::window::give_back_focus(prev);
                }
                if !self.hovered.get() {
                    self.on_hover(false); // close as usual now
                }
            }
        }
    }

    pub fn on_month(&self, year: i32, month: u32, events: &[crate::services::calendar::Meeting]) {
        self.calendar.set_month(year, month, events);
    }

    /// Once a minute, to keep relative times ("in 5 min") fresh.
    pub fn tick_calendar(&self) {
        self.calendar.tick();
    }

    pub fn on_notifications(&self, items: &[crate::services::notifications::Notification]) {
        self.notifications.update(items);
    }

    pub fn on_controls(&self, state: &crate::services::controls::State) {
        self.controls.update(state);
    }

    pub fn on_shelf(&self, items: &[std::path::PathBuf]) {
        self.shelf.update(items);
    }

    fn set_dragging(self: &Rc<Self>, active: bool) {
        self.dragging.set(active);
        if !active && !self.hovered.get() {
            self.on_hover(false); // close as usual now
        }
    }

    /// The whole notch accepts dropped files: dragging one over it opens the
    /// shelf (even from collapsed), dropping adds it there.
    fn accept_drops(self: &Rc<Self>, commands: async_channel::Sender<crate::shelf::Command>) {
        if !self.features.shelf {
            return;
        }
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        target.connect_enter(move |_, _, _| {
            if let Some(i) = weak.upgrade() {
                i.dragging.set(true);
                i.show_named_page("shelf");
            }
            gdk::DragAction::COPY
        });
        let weak = Rc::downgrade(self);
        target.connect_leave(move |_| {
            if let Some(i) = weak.upgrade() {
                i.set_dragging(false);
            }
        });
        let weak = Rc::downgrade(self);
        target.connect_drop(move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths: Vec<_> = list.files().iter().filter_map(|f| f.path()).collect();
            if paths.is_empty() {
                return false; // e.g. remote URIs with no local path
            }
            let _ = commands.try_send(crate::shelf::Command::Add(paths));
            if let Some(i) = weak.upgrade() {
                i.set_dragging(false);
            }
            true
        });
        self.root.add_controller(target);
    }

    /// Opens (or switches to) the expanded card named `page`.
    fn show_named_page(self: &Rc<Self>, page: &str) {
        let Some(i) = self.pages().iter().position(|(name, _)| *name == page) else {
            return;
        };
        if self.state.get() == State::Expanded && self.page.get() == i {
            return;
        }
        self.cancel_peek_timer();
        self.page.set(i);
        self.expand();
    }

    pub fn on_todo(&self, view: &crate::todo::View) {
        self.todo.update(view);
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

    /// Translation switched from any island's button: all follow, and the
    /// open lyrics are fetched again with (or without) English lines.
    pub fn set_translate(self: &Rc<Self>, on: bool) {
        if self.translate.replace(on) == on {
            return;
        }
        if on {
            self.translate_button.add_css_class("on");
        } else {
            self.translate_button.remove_css_class("on");
        }
        self.lyrics_loaded.replace(None);
        self.request_lyrics();
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
            translate: self.translate.get(),
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
    /// Expanded cards in the user's order (Settings → Card order). The
    /// "music" slot is the clock card when nothing plays.
    fn pages(&self) -> Vec<(&'static str, (f64, f64))> {
        let f = &self.features;
        let mut pages = Vec::new();
        for name in &f.order {
            let page = match name.as_str() {
                "music" if self.has_music.get() && self.lyrics_mode.get() => {
                    ("music", EXPANDED_LYRICS)
                }
                "music" if self.has_music.get() => ("music", EXPANDED_MUSIC),
                "music" => ("idle", EXPANDED_IDLE),
                "controls" if f.controls => ("controls", EXPANDED_CONTROLS),
                "notifications" if f.notifications => ("notifications", EXPANDED_NOTIFICATIONS),
                "calendar" if f.calendar => ("calendar", EXPANDED_CALENDAR),
                "water" if f.water => ("water", EXPANDED_WATER),
                "todo" if f.todo => ("todo", EXPANDED_TODO),
                "shelf" if f.shelf => ("shelf", EXPANDED_SHELF),
                _ => continue,
            };
            pages.push(page);
        }
        if pages.is_empty() {
            pages.push(("idle", EXPANDED_IDLE)); // never leave the notch empty
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
        let lyrics_open = visible && self.expanded_page() == ("music", EXPANDED_LYRICS);
        self.translate_button.set_visible(lyrics_open);
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
        self.translate_button.set_margin_end(inset);
        self.translate_button.set_margin_top(top + 34);
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
            self.root.set_visible(true); // even over a fullscreen window
            self.claude.show(r);
            self.fit_claude();
            return;
        }
        if self.state.get() != State::Claude {
            return;
        }
        self.claude.stop();
        if self.fullscreen.get() {
            self.state.set(State::Expanded);
            self.collapse();
            self.set_fullscreen(true);
            return;
        }
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
        if self.fullscreen.get() {
            return; // don't pop over videos or slides
        }
        self.peek_page.set(match &peek.kind {
            Kind::Notification { .. } => Some("notifications"),
            Kind::Meeting => Some("calendar"),
            Kind::Todo => Some("todo"),
            Kind::Water => Some("water"),
            Kind::Bluetooth { .. } | Kind::Charging | Kind::Unplugged => Some("controls"),
            _ => None,
        });
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
        if !self.root.is_visible() {
            return; // hidden for fullscreen: keep the empty input region
        }
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

/// The shelf's command channel, reused by the island-wide drop target.
fn shelf_commands_for_drop(island: &Island) -> async_channel::Sender<crate::shelf::Command> {
    island.shelf_commands.clone()
}
