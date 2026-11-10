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
// Cards end ~10px above the bottom edge. The tabs, clock and settings button
// share the top row of every card, at a fixed spot: the notch hangs from the
// top of the screen, so things there never move when cards change size.
/// Top of the shared row, matching the cards' 16px top padding.
const TOP_ROW_Y: i32 = 32;
/// The clock sits above the tabs, centered in the strip over the GNOME bar,
/// where it also is when the notch is closed.
const CLOCK_Y: i32 = 4;
/// Height of that row (tabs, gear, clock all share it).
const TOP_ROW_H: i32 = 26;
/// Top of the ‹ › page arrows, the same on every card.
const ARROWS_Y: i32 = 96;
/// The tab strip starts this far left of the notch's center, on every card.
/// Centering it instead would shift every tab whenever the named tab's
/// label changes length.
const TABS_LEFT_OF_CENTER: i32 = 170;
/// x, y, width, height in island coordinates.
type Rect = (f64, f64, f64, f64);

/// Forward (to a card further right) slides the content left, back slides
/// it right, so the motion matches where the card sits in the tabs.
fn slide_toward(forward: bool) -> StackTransitionType {
    if forward {
        StackTransitionType::SlideLeft
    } else {
        StackTransitionType::SlideRight
    }
}

/// Icon and name for a card's tab.
fn tab(page: &str) -> (&'static str, &'static str) {
    match page {
        "music" => ("atoll-headphones-symbolic", "Music"),
        "idle" => ("atoll-clock-symbolic", "Clock"),
        "controls" => ("atoll-controls-symbolic", "Controls"),
        "notifications" => ("atoll-bell-symbolic", "Notifications"),
        "calendar" => ("atoll-event-symbolic", "Calendar"),
        "water" => ("atoll-drop-symbolic", "Water"),
        "todo" => ("atoll-tasks-symbolic", "To-do"),
        "shelf" => ("atoll-tray-symbolic", "Shelf"),
        _ => ("atoll-circle-symbolic", ""),
    }
}

/// How long "no track" must last before the music card goes away.
const MUSIC_GONE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
pub const EXPANDED_MUSIC: (f64, f64) = (568.0, 244.0);
pub const EXPANDED_LYRICS: (f64, f64) = (868.0, 244.0);
pub const EXPANDED_WATER: (f64, f64) = (568.0, 228.0);
pub const EXPANDED_IDLE: (f64, f64) = (568.0, 162.0);
pub const EXPANDED_TODO: (f64, f64) = (568.0, 228.0);
/// Wider than the rest: month grid and day agenda side by side.
pub const EXPANDED_CALENDAR: (f64, f64) = (680.0, 274.0);
pub const EXPANDED_SHELF: (f64, f64) = (568.0, 228.0);
pub const EXPANDED_CONTROLS: (f64, f64) = (568.0, 228.0);
pub const EXPANDED_NOTIFICATIONS: (f64, f64) = (568.0, 282.0);
/// No notifications: as short as the other cards.
pub const EXPANDED_NOTIFICATIONS_EMPTY: (f64, f64) = (568.0, 228.0);
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
/// Card-to-card slide: long enough to read as movement, short enough not
/// to hold up the next click.
const SLIDE_MS: u32 = 240;
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
    /// `None` clears all; `Some(id)` removes one (opened from the card).
    pub clear_notifications: async_channel::Sender<Option<u64>>,
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
    /// The current tab's pink pill, drawn under the tabs so it can glide
    /// from one tab to the next.
    indicator: DrawingArea,
    /// Where the pill is drawn now (x, y, w, h in island coordinates).
    pill: Rc<Cell<Option<Rect>>>,
    pill_tick: RefCell<Option<gtk::TickCallbackId>>,
    /// The tab buttons, and the cards they were built for.
    #[allow(clippy::type_complexity)]
    tabs: RefCell<(Vec<&'static str>, Vec<(gtk::Button, gtk::Revealer)>)>,
    /// Sizes the notifications card (short when there's nothing).
    notifications_empty: Cell<bool>,
    /// Direction of the card switch in progress, for the slide.
    slide: Cell<Option<StackTransitionType>>,
    /// The island itself, for the tab buttons (rebuilt on every page).
    me: RefCell<std::rc::Weak<Island>>,
    gear: gtk::Button,
    /// Shared top row: the time, left of the settings button.
    clock: gtk::Label,
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
    /// Closed notch in lyrics mode: follows the line being sung.
    lyric_tick: RefCell<Option<glib::SourceId>>,
    /// Pinned: stays open when the pointer leaves, e.g. to read lyrics while
    /// working.
    pinned: Cell<bool>,
    /// Pending "music stopped": players report no track for a moment
    /// between songs, which must not close or unpin the card.
    music_gone: RefCell<Option<glib::SourceId>>,
    /// Collapsed width: wider while the song's name takes its turn.
    collapsed_w: Cell<f64>,
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
            // One for every card that takes typing (to-do, Wi-Fi password,
            // Claude replies).
            let w = weak.clone();
            let on_typing: Rc<dyn Fn(Typing)> = Rc::new(move |t| {
                if let Some(island) = w.upgrade() {
                    island.on_typing(t);
                }
            });
            let w = weak.clone();
            let on_drag = move |active| {
                if let Some(island) = w.upgrade() {
                    island.set_dragging(active);
                }
            };
            Self::build(hit, deps, on_lyrics, on_pin, on_typing, on_drag)
        });
        island.connect_pager();
        let weak = Rc::downgrade(&island);
        island.collapsed.connect_width(move |w| {
            if let Some(i) = weak.upgrade() {
                i.set_collapsed_width(w);
            }
        });
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
        on_typing: Rc<dyn Fn(Typing)>,
        on_drag: impl Fn(bool) + 'static,
    ) -> Self {
        let typing = |t: &Rc<dyn Fn(Typing)>| {
            let t = t.clone();
            move |x| t(x)
        };
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
        let claude = ClaudeCard::new(claude_answers, typing(&on_typing));
        let todo = TodoCard::new(todo_commands, typing(&on_typing));

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
        let controls = ControlsCard::new(controls_commands, typing(&on_typing));
        stack.add_named(&controls.root, Some("controls"));
        let notifications = NotificationsCard::new(move |which| {
            let _ = clear_notifications.try_send(which);
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
        // Card tabs: an icon per card, the current one named; click to jump.
        let dots = gtk::Box::builder()
            .css_classes(["tabs"])
            .spacing(4)
            .halign(Align::Start)
            .margin_start(WIN_W / 2 - TABS_LEFT_OF_CENTER)
            .valign(Align::Start)
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
        // Added before the tabs, so it's drawn beneath them.
        let pill: Rc<Cell<Option<Rect>>> = Rc::default();
        let indicator = DrawingArea::builder().can_target(false).build();
        let p = pill.clone();
        indicator.set_draw_func(move |_, cr, _, _| {
            let Some((x, y, w, h)) = p.get() else { return };
            let r = h / 2.0;
            cr.new_sub_path();
            cr.arc(
                x + w - r,
                y + r,
                r,
                -std::f64::consts::FRAC_PI_2,
                std::f64::consts::FRAC_PI_2,
            );
            cr.arc(
                x + r,
                y + r,
                r,
                std::f64::consts::FRAC_PI_2,
                3.0 * std::f64::consts::FRAC_PI_2,
            );
            cr.close_path();
            let (pr, pg, pb) = crate::palette::pink();
            cr.set_source_rgba(pr, pg, pb, 0.2);
            let _ = cr.fill();
        });
        root.add_overlay(&indicator);
        root.add_overlay(&dots);
        root.add_overlay(&gear);
        let clock = gtk::Label::builder()
            .css_classes(["clock"])
            .halign(Align::Center)
            .valign(Align::Start)
            .margin_top(CLOCK_Y)
            // Same height as the tabs and gear, text centered in it.
            .height_request(TOP_ROW_H)
            .yalign(0.5)
            .can_target(false)
            .visible(false)
            .build();
        crate::cards::live_clock(&clock, "%H:%M:%S");
        root.add_overlay(&clock);
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
            indicator,
            pill,
            pill_tick: RefCell::new(None),
            tabs: RefCell::default(),
            notifications_empty: Cell::new(true),
            slide: Cell::new(None),
            me: RefCell::new(std::rc::Weak::new()),
            gear,
            clock,
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
            lyric_tick: RefCell::new(None),
            pinned: Cell::new(false),
            music_gone: RefCell::new(None),
            collapsed_w: Cell::new(COLLAPSED.0),
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

    /// Renders the island as it looks now to a PNG (see `preview`).
    /// Returns false when there was nothing drawn yet (try again later).
    pub fn save_png(&self, path: &std::path::Path) -> bool {
        let root = self.root.upcast_ref::<gtk::Widget>();
        // Off screen, GTK's layout pass may not have run yet (it waits for
        // frames the compositor never sends): lay out now.
        let (w, h) = (WIN_W, WIN_H);
        root.measure(gtk::Orientation::Horizontal, -1);
        root.allocate(w, h, -1, None);
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(root)).snapshot(&snapshot, f64::from(w), f64::from(h));
        let Some(node) = snapshot.to_node() else {
            tracing::warn!(
                "preview: nothing to render (mapped {}, visible {}, {w}x{h}, page {:?})",
                root.is_mapped(),
                root.is_visible(),
                self.stack.visible_child_name()
            );
            return false;
        };
        let Some(renderer) = root.native().and_then(|n| n.renderer()) else {
            return false;
        };
        let texture = renderer.render_texture(node, None);
        match texture.save_to_png(path) {
            Ok(()) => tracing::info!("preview saved to {}", path.display()),
            Err(e) => tracing::warn!("preview: cannot save {}: {e}", path.display()),
        }
        true
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
            // Fake prompts, preview only (no hook is waiting on them).
            "claude-permission" | "claude-question" if crate::preview::active() => {
                let request = if page == "claude-permission" {
                    claude::Request::Permission {
                        tool: "Bash".into(),
                        summary: "mv loyalty_points_on_payment sejatibrand_loyalty_points && \
                                  grep -rn loyalty_points_on_payment ."
                            .into(),
                        project: "19.0-sejatibrands-6634496-sudnu".into(),
                        always: Some("mv:* · this project".into()),
                    }
                } else {
                    claude::Request::Question {
                        questions: vec![claude::Question {
                            question: "Which lyrics source should Atoll try next?".into(),
                            header: "Lyrics".into(),
                            options: vec![
                                claude::Choice {
                                    label: "LRCLIB only".into(),
                                    description: "Keep the current single source".into(),
                                },
                                claude::Choice {
                                    label: "Add a fallback".into(),
                                    description: "Try a second source when LRCLIB has none".into(),
                                },
                                claude::Choice {
                                    label: "Different tool".into(),
                                    description: "Use another method instead".into(),
                                },
                                claude::Choice {
                                    label: "Plan is fine".into(),
                                    description: "Keep the plan as written".into(),
                                },
                            ],
                            multi_select: false,
                        }],
                        project: "atoll".into(),
                    }
                };
                self.on_claude(Some(&request));
                return;
            }
            // Closed notch showing a lyric line (preview only).
            "collapsed-lyric" if crate::preview::active() => {
                self.collapsed.set_lyric(Some(
                    "And I'm stuck in yesterday, for me it's still the same",
                ));
                return;
            }
            "lyrics" => {
                if !self.lyrics_mode.get() {
                    self.toggle_lyrics();
                }
                "music"
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
        // A small pill over the bar is easy to miss by a few pixels; the
        // nearest button within reach takes the click, like a larger
        // touch target would.
        for dx in [0.0, -6.0, 6.0, -12.0, 12.0] {
            if let Some(button) = self.button_at(f64::from(x) + dx, y) {
                if button.is_sensitive() {
                    button.emit_clicked();
                }
                return;
            }
        }
    }

    fn button_at(&self, x: f64, y: f64) -> Option<gtk::Button> {
        let mut widget = self.root.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(w) = widget {
            if let Ok(button) = w.clone().downcast::<gtk::Button>() {
                return Some(button);
            }
            widget = w.parent();
        }
        None
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

    pub fn on_notifications(
        self: &Rc<Self>,
        items: &[crate::services::notifications::Notification],
    ) {
        self.notifications.update(items);
        let was = self.notifications_empty.replace(items.is_empty());
        // Open on this card and it just changed size: refit the notch.
        if was != items.is_empty()
            && self.state.get() == State::Expanded
            && self.expanded_page().0 == "notifications"
        {
            self.expand();
        }
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
                if let Some(id) = self.music_gone.take() {
                    id.remove();
                }
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
                self.sync_lyric(); // play/pause starts or stops it
            }
            Update::Track(None) => {
                if self.music_gone.borrow().is_some() || !self.has_music.get() {
                    return;
                }
                let weak = Rc::downgrade(self);
                let id = glib::timeout_add_local_once(MUSIC_GONE_GRACE, move || {
                    if let Some(island) = weak.upgrade() {
                        island.music_gone.take();
                        island.music_stopped();
                    }
                });
                self.music_gone.replace(Some(id));
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
            Some(lines) => Lyrics::Lines(lines, update.synced),
            None if update.failed => {
                // Not an answer: let the next open of the panel ask again.
                self.lyrics_loaded.replace(None);
                Lyrics::Unreachable
            }
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

    /// While closed, playing and in lyrics mode, the closed notch shows the
    /// line being sung; checked a few times a second, only then (no timer
    /// otherwise).
    fn sync_lyric(self: &Rc<Self>) {
        if crate::preview::active() {
            return; // previews set the line themselves
        }
        let wanted = |i: &Island| {
            i.state.get() == State::Collapsed
                && i.lyrics_mode.get()
                && i.has_music.get()
                && i.music.is_playing()
        };
        if !wanted(self) {
            if let Some(id) = self.lyric_tick.take() {
                id.remove();
            }
            self.collapsed.set_lyric(None);
            return;
        }
        if self.lyric_tick.borrow().is_some() {
            return;
        }
        let show = |i: &Rc<Island>| {
            let line = i.music.current_line();
            i.collapsed.set_lyric(line.as_deref());
        };
        show(self);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(300), move || {
            let Some(i) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !wanted(&i) {
                i.lyric_tick.take();
                i.collapsed.set_lyric(None);
                return glib::ControlFlow::Break;
            }
            show(&i);
            glib::ControlFlow::Continue
        });
        self.lyric_tick.replace(Some(id));
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

    fn music_stopped(self: &Rc<Self>) {
        self.art_url.replace(None);
        self.set_art(None);
        self.collapsed.set_track(None);
        self.lyrics_key.replace(None);
        self.set_has_music(false);
        if self.pinned.get() {
            self.toggle_pin();
        }
        self.sync_lyric();
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
                "notifications" if f.notifications => (
                    "notifications",
                    if self.notifications_empty.get() {
                        EXPANDED_NOTIFICATIONS_EMPTY
                    } else {
                        EXPANDED_NOTIFICATIONS
                    },
                ),
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

    /// The current card and the notch size for it.
    fn expanded_page(&self) -> (&'static str, (f64, f64)) {
        let pages = self.pages();
        let i = self.page.get().min(pages.len() - 1);
        pages[i]
    }

    fn go_to(self: &Rc<Self>, i: usize) {
        if self.state.get() != State::Expanded || self.page.get() == i {
            return;
        }
        self.slide.set(Some(slide_toward(i > self.page.get())));
        self.page.set(i);
        self.expand();
    }

    /// Steps to the next/previous card, wrapping around.
    fn go(self: &Rc<Self>, delta: isize) {
        let n = self.pages().len();
        if n < 2 || self.state.get() != State::Expanded || self.tick.borrow().is_some() {
            return;
        }
        let i = (self.page.get().min(n - 1) as isize + delta).rem_euclid(n as isize);
        self.slide.set(Some(slide_toward(delta > 0)));
        self.page.set(i as usize);
        self.expand();
    }

    fn connect_pager(self: &Rc<Self>) {
        self.me.replace(Rc::downgrade(self));
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
        // The clock card shows the time big already.
        self.clock
            .set_visible(visible && self.expanded_page().0 != "idle");
        if visible {
            let w = self.expanded_page().1 .0;
            // Same column as the › arrow below it.
            self.gear.set_margin_end((WIN_W - w as i32) / 2 + 8);
            // On the tab row.
            self.gear.set_margin_top(TOP_ROW_Y);
        }
        let lyrics_open = visible && self.expanded_page() == ("music", EXPANDED_LYRICS);
        self.translate_button.set_visible(lyrics_open);
        let pages = self.pages();
        let show = visible && pages.len() > 1;
        self.prev.set_visible(show);
        self.next.set_visible(show);
        self.dots.set_visible(show);
        self.indicator.set_visible(show);
        if !show {
            return;
        }
        let w = self.expanded_page().1 .0;
        let inset = (WIN_W - w as i32) / 2 + 8;
        // One height on every card (centering on the content made them jump
        // as cards changed size); below the GNOME bar, where clicks reach us.
        let top = ARROWS_Y;
        self.prev.set_margin_start(inset);
        self.prev.set_margin_top(top);
        self.next.set_margin_end(inset);
        self.next.set_margin_top(top);
        self.translate_button.set_margin_end(inset);
        self.translate_button.set_margin_top(top + 48);
        self.fill_tabs();
    }

    /// One tab per card, the current one named. The strip is centered at the
    /// top, so it stays put while the notch morphs between cards.
    fn fill_tabs(&self) {
        let pages = self.pages();
        self.dots.set_margin_top(TOP_ROW_Y);
        let names: Vec<&'static str> = pages.iter().map(|(n, _)| *n).collect();
        // Built once per set of cards and kept, so a switch can animate
        // from the old tab to the new one instead of redrawing the row.
        if self.tabs.borrow().0 != names {
            while let Some(child) = self.dots.first_child() {
                self.dots.remove(&child);
            }
            let mut built = Vec::new();
            for (i, name) in names.iter().enumerate() {
                let (icon, label) = tab(name);
                let button = control_button(&["tab"], None);
                let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                content.append(&gtk::Image::from_icon_name(icon));
                // The name slides open on the current tab and closed on the
                // one being left, so the highlight appears to travel.
                let name_label = gtk::Label::builder().label(label).margin_start(6).build();
                let reveal = gtk::Revealer::builder()
                    .transition_type(gtk::RevealerTransitionType::SlideRight)
                    .transition_duration(SLIDE_MS)
                    .child(&name_label)
                    .build();
                content.append(&reveal);
                button.set_child(Some(&content));
                button.set_tooltip_text(Some(label));
                let me = self.me.borrow().clone();
                button.connect_clicked(move |_| {
                    if let Some(island) = me.upgrade() {
                        island.go_to(i);
                    }
                });
                self.dots.append(&button);
                built.push((button, reveal));
            }
            self.tabs.replace((names.clone(), built));
        }
        let current = self.page.get().min(names.len().saturating_sub(1));
        for (i, (button, reveal)) in self.tabs.borrow().1.iter().enumerate() {
            let on = i == current;
            if on {
                button.add_css_class("current");
            } else {
                button.remove_css_class("current");
            }
            // The current tab already shows its name.
            button.set_has_tooltip(!on);
            reveal.set_reveal_child(on);
        }
        self.glide_pill();
    }

    /// Moves the pink pill to the current tab: from where it is, it glides
    /// (position and width together, as the tab's name opens); the first
    /// time it simply appears there.
    fn glide_pill(&self) {
        self.indicator.set_visible(true);
        let Some(me) = self.me.borrow().upgrade() else {
            return;
        };
        if let Some(id) = self.pill_tick.take() {
            id.remove();
        }
        let from = self.pill.get();
        let start = Cell::new(None::<i64>);
        let weak = Rc::downgrade(&me);
        let id = self.indicator.add_tick_callback(move |_, clock| {
            let Some(island) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let now = clock.frame_time();
            let t0 = *start.get().get_or_insert(now);
            start.set(Some(t0));
            let t = ((now - t0) as f64 / (f64::from(SLIDE_MS) * 1000.0)).clamp(0.0, 1.0);
            // The target is read every frame: the new tab widens while its
            // name slides open.
            let Some(to) = island.current_tab_rect() else {
                return glib::ControlFlow::Continue;
            };
            let e = ease_out(t);
            let lerp = |a: f64, b: f64| a + (b - a) * e;
            let rect = match from {
                Some(f) => (
                    lerp(f.0, to.0),
                    lerp(f.1, to.1),
                    lerp(f.2, to.2),
                    lerp(f.3, to.3),
                ),
                None => to,
            };
            island.pill.set(Some(rect));
            island.indicator.queue_draw();
            if t >= 1.0 {
                island.pill_tick.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.pill_tick.replace(Some(id));
    }

    fn current_tab_rect(&self) -> Option<Rect> {
        let tabs = self.tabs.borrow();
        let button = tabs
            .1
            .get(self.page.get().min(tabs.1.len().checked_sub(1)?))?;
        let b = button.0.compute_bounds(&self.root)?;
        Some((
            f64::from(b.x()),
            f64::from(b.y()),
            f64::from(b.width()),
            f64::from(b.height()),
        ))
    }

    fn expand(self: &Rc<Self>) {
        let switching = self.state.replace(State::Expanded) == State::Expanded;
        self.sync_lyric();
        if !switching {
            // Opening fresh: the pill appears on its tab, no glide.
            self.pill.set(None);
        }
        self.show_pager(false);
        if switching && self.pages().len() > 1 {
            // Moving between cards: the tabs stay where the pointer is and
            // only the highlight moves.
            self.dots.set_visible(true);
            self.fill_tabs();
        }
        let (page, target) = self.expanded_page();
        self.set_bounds(target);
        if switching && self.size.get() == target && self.tick.borrow().is_none() {
            // Same size: a straight carousel, old card out, new card in.
            if let Some(slide) = self.slide.take() {
                self.stack.set_transition_duration(SLIDE_MS);
                self.stack.set_visible_child_full(page, slide);
                self.music.set_active(page == "music");
                self.show_pager(true);
                return;
            }
        }
        // Different size: the outline morphs first (content inside a moving
        // outline would spill out), then the card slides in (`on_settled`).
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
        let size = self.collapsed_size();
        self.collapsed.set_width(size.0);
        self.set_bounds(size);
        self.show_page("none", false);
        self.animate_to(size);
    }

    fn collapsed_size(&self) -> (f64, f64) {
        (self.collapsed_w.get(), COLLAPSED.1)
    }

    /// The collapsed content asks for a new width (the song's turn). While
    /// collapsed the outline morphs there; the content keeps showing, laid
    /// out at whichever of the two widths is narrower until the outline
    /// settles, so nothing ever spills outside it.
    fn set_collapsed_width(self: &Rc<Self>, w: f64) {
        if self.collapsed_w.replace(w) == w {
            return;
        }
        if self.state.get() != State::Collapsed {
            return; // used on the next collapse
        }
        let current = self.size.get().0;
        self.collapsed.set_width(w.min(current));
        self.set_bounds((w.max(current), COLLAPSED.1));
        self.animate_to(self.collapsed_size());
    }

    /// Content appears only once the notch has reached its size; growing the
    /// outline around fixed-size content would spill it outside the shape.
    fn on_settled(self: &Rc<Self>) {
        match self.state.get() {
            State::Collapsed => {
                self.sync_lyric();
                self.collapsed.set_width(self.collapsed_w.get());
                self.set_bounds(self.collapsed_size());
                self.show_page("collapsed", true);
                let queued = self.queued_peek.take();
                if let Some((peek, at)) = queued {
                    if at.elapsed() < PEEK_STALE {
                        self.on_peek(peek);
                    }
                }
            }
            State::Peek => self.show_page("peek", true),
            State::Claude => {
                self.show_page("claude", true);
                // Measured before GTK had styled the card, the first height
                // can be far too generous; once it's on screen, tighten.
                let weak = Rc::downgrade(self);
                glib::idle_add_local_once(move || {
                    let Some(i) = weak.upgrade() else { return };
                    if i.state.get() != State::Claude {
                        return;
                    }
                    let h = f64::from(i.claude.natural_height())
                        .clamp(EXPANDED_CLAUDE.1 * 0.75, CLAUDE_MAX_H);
                    if (h - i.size.get().1).abs() > 2.0 {
                        let size = (EXPANDED_CLAUDE.0, h);
                        i.set_bounds(size);
                        i.animate_to(size);
                    }
                });
            }
            State::Expanded => {
                let (page, _) = self.expanded_page();
                match self.slide.take() {
                    Some(slide) => {
                        self.stack.set_transition_duration(SLIDE_MS);
                        self.stack.set_visible_child_full(page, slide);
                    }
                    None => self.show_page(page, true),
                }
                self.music.set_active(page == "music");
                self.show_pager(true);
            }
        }
    }

    fn show_page(&self, name: &str, fade: bool) {
        self.stack.set_transition_duration(CONTENT_FADE_MS);
        let transition = if fade {
            StackTransitionType::Crossfade
        } else {
            StackTransitionType::None
        };
        self.stack.set_visible_child_full(name, transition);
    }

    fn animate_to(self: &Rc<Self>, to: (f64, f64)) {
        // Off screen (preview) the compositor sends no frames, so an
        // animation would never finish: jump instead.
        let clock = self
            .notch
            .frame_clock()
            .filter(|_| !crate::preview::active());
        let Some(clock) = clock else {
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
