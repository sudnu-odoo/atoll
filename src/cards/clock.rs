//! The collapsed notch (time, plus a small now-playing hint) and the
//! expanded card shown when nothing is playing.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{glib, Align, Label, Orientation, Stack, StackTransitionType};

use super::widgets::{Eyes, Meter, Mood, Ring, TrackCell};
use super::{every_minute, live_clock};
use crate::services::mpris::NowPlaying;

/// The eyes own the center; the time takes a turn this long at each new
/// minute (and when the mood changes). While music plays, the time, the
/// eyes and the song take turns, this long each.
const TURN: Duration = Duration::from_secs(5);
const CROSSFADE_MS: u32 = 350;
/// Room either side of the song for the progress ring and the meter.
const SONG_SIDES: f64 = 96.0;
/// Widest the notch grows for a song; longer names end in "…".
const SONG_MAX_W: f64 = 560.0;
/// Widest it grows for a lyric line (lyrics mode, notch closed).
const LYRIC_MAX_W: f64 = 720.0;

type WidthFn = Box<dyn Fn(f64)>;

pub struct Collapsed {
    pub root: gtk::CenterBox,
    center: Stack,
    song: Label,
    /// The line being sung, while lyrics mode is on and the notch closed.
    lyric: Label,
    lyric_on: Cell<bool>,
    ring: Rc<Ring>,
    meter: Rc<Meter>,
    track: TrackCell,
    eyes: Rc<Eyes>,
    /// Eyes switched off in settings: they never take a turn.
    eyes_enabled: bool,
    mood: Cell<Option<Mood>>,
    turn: RefCell<Option<glib::SourceId>>,
    /// Told the width the notch should have for the page now showing.
    on_width: RefCell<Option<WidthFn>>,
}

impl Collapsed {
    pub fn new(eyes_enabled: bool) -> Rc<Self> {
        let time = Label::builder().css_classes(["clock"]).build();
        let song = Label::builder()
            .css_classes(["collapsed-song"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(52)
            .build();
        super::full_text_on_hover(&song);
        let lyric = Label::builder()
            .css_classes(["collapsed-song"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(80)
            .build();
        let track = TrackCell::default();
        let ring = Ring::new(track.clone(), 16);
        let meter = Meter::new();
        ring.area.set_visible(false);
        meter.area.set_visible(false);

        let eyes = Eyes::new();
        let face = gtk::Box::builder()
            .css_classes(["eyes"])
            .spacing(14)
            .halign(Align::Center)
            .valign(Align::Center)
            .build();
        face.append(&eyes.left);
        face.append(&eyes.right);

        // Hidden pages are unmapped, so the eyes stop animating whenever
        // something else is showing.
        let center = Stack::builder()
            .transition_type(StackTransitionType::Crossfade)
            .transition_duration(CROSSFADE_MS)
            .hhomogeneous(false)
            .interpolate_size(false)
            .build();
        center.add_named(&time, Some("time"));
        center.add_named(&face, Some("eyes"));
        center.add_named(&song, Some("song"));
        center.add_named(&lyric, Some("lyric"));
        center.set_visible_child_name("time");

        let root = gtk::CenterBox::builder()
            .css_classes(["collapsed"])
            .width_request(crate::island::COLLAPSED.0 as i32)
            .height_request(crate::island::COLLAPSED.1 as i32)
            .start_widget(&ring.area)
            .center_widget(&center)
            .end_widget(&meter.area)
            .build();

        let collapsed = Rc::new(Self {
            root,
            center,
            song,
            lyric,
            lyric_on: Cell::new(false),
            ring,
            meter,
            track,
            eyes,
            eyes_enabled,
            // No update arrives when nothing plays at startup, so begin sleepy.
            mood: Cell::new(Some(Mood::Sleepy)),
            turn: RefCell::new(None),
            on_width: RefCell::new(None),
        });
        collapsed.show_turn("time");
        let weak = Rc::downgrade(&collapsed);
        every_minute(move |now| {
            if let Ok(s) = now.format("%H:%M") {
                time.set_label(&s);
            }
            // Idle: the time comes back each minute. Playing: the turns
            // already include it.
            if let Some(c) = weak.upgrade() {
                if !c.playing() {
                    c.show_turn("time");
                }
            }
        });
        collapsed
    }

    pub fn connect_width(&self, f: impl Fn(f64) + 'static) {
        self.on_width.replace(Some(Box::new(f)));
    }

    /// Sets the width the content lays out in (the island calls this when
    /// the notch outline is at, or heading to, that width).
    pub fn set_width(&self, w: f64) {
        self.root.set_width_request(w as i32);
    }

    fn playing(&self) -> bool {
        self.mood.get() == Some(Mood::Excited)
    }

    pub fn set_track(self: &Rc<Self>, np: Option<&NowPlaying>) {
        self.ring.area.set_visible(np.is_some());
        self.meter.area.set_visible(np.is_some());
        self.track.replace(np.cloned());
        self.ring.refresh();
        self.meter.set_playing(np.is_some_and(|n| n.playing));
        let song = np.map_or(String::new(), |n| {
            if n.artist.is_empty() {
                n.title.clone()
            } else {
                format!("{}  ·  {}", n.title, n.artist)
            }
        });
        if self.song.label() != song {
            self.song.set_label(&song);
            if self.center.visible_child_name().as_deref() == Some("song") {
                self.report_width("song");
            }
        }

        let mood = if np.is_some_and(|n| n.playing) {
            Mood::Excited
        } else {
            Mood::Sleepy
        };
        if self.mood.replace(Some(mood)) != Some(mood) {
            self.eyes.set_mood(mood);
            self.show_turn("time"); // the time once, then the new mood
        }
    }

    /// The page after `page` in the current rotation, or `None` to stay.
    fn next(&self, page: &str) -> Option<&'static str> {
        let has_song = !self.song.label().is_empty();
        let order: &[&'static str] = match (self.playing(), self.eyes_enabled, has_song) {
            (true, true, true) => &["time", "eyes", "song"],
            (true, false, true) => &["time", "song"],
            (true, true, false) => &["time", "eyes"],
            // Idle: the eyes take over after the time and stay.
            (false, true, _) => return (page == "time").then_some("eyes"),
            _ => return None,
        };
        let i = order.iter().position(|p| *p == page).unwrap_or(0);
        Some(order[(i + 1) % order.len()])
    }

    /// Lyrics mode with the notch closed: the line being sung takes the
    /// center (no turns), and the notch fits its width. `None` hands back
    /// to the usual turns.
    pub fn set_lyric(self: &Rc<Self>, line: Option<&str>) {
        match line {
            Some(text) => {
                if !self.lyric_on.replace(true) {
                    if let Some(id) = self.turn.take() {
                        id.remove();
                    }
                }
                if self.lyric.label() != text {
                    self.lyric.set_label(text);
                    self.center.set_visible_child_name("lyric");
                    self.report_width("lyric");
                }
            }
            None => {
                if self.lyric_on.replace(false) {
                    self.lyric.set_label("");
                    self.show_turn("time");
                }
            }
        }
    }

    fn show_turn(self: &Rc<Self>, page: &'static str) {
        if self.lyric_on.get() {
            return; // the lyric holds the center
        }
        if let Some(id) = self.turn.take() {
            id.remove();
        }
        self.center.set_visible_child_name(page);
        self.report_width(page);
        let Some(next) = self.next(page) else { return };
        let stays = !self.playing() && next == "eyes";
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(TURN, move || {
            let Some(c) = weak.upgrade() else { return };
            c.turn.take();
            if stays {
                // Idle eyes stay until the next minute; no timer runs.
                c.center.set_visible_child_name("eyes");
                c.report_width("eyes");
            } else {
                c.show_turn(next);
            }
        });
        self.turn.replace(Some(id));
    }

    fn report_width(&self, page: &str) {
        let base = crate::island::COLLAPSED.0;
        let w = match page {
            "song" => {
                let (_, natural, _, _) = self.song.measure(Orientation::Horizontal, -1);
                (f64::from(natural) + SONG_SIDES).clamp(base, SONG_MAX_W)
            }
            "lyric" => {
                let (_, natural, _, _) = self.lyric.measure(Orientation::Horizontal, -1);
                (f64::from(natural) + SONG_SIDES).clamp(base, LYRIC_MAX_W)
            }
            _ => base,
        };
        if let Some(f) = self.on_width.borrow().as_ref() {
            f(w);
        }
    }
}

pub struct Idle {
    pub root: gtk::Box,
}

impl Idle {
    pub fn new() -> Self {
        let time = Label::builder().css_classes(["big-clock"]).build();
        live_clock(&time, "%H:%M:%S");
        let date = Label::builder().css_classes(["caps"]).build();

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["idle-card"])
            .width_request(crate::island::EXPANDED_IDLE.0 as i32)
            .height_request(crate::island::EXPANDED_IDLE.1 as i32)
            .valign(Align::Start)
            .build();
        // Empty slot for the island's shared row (tabs, settings).
        root.append(&gtk::Box::builder().css_classes(["card-top"]).build());
        root.append(&time);
        root.append(&date);

        every_minute(move |now: &glib::DateTime| {
            if let Ok(s) = now.format("%A  %d %B") {
                date.set_label(&s.to_uppercase());
            }
        });
        Self { root }
    }
}
