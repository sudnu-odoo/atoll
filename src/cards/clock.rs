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
/// minute (and when the mood changes). While music plays they simply take
/// turns, this long each.
const TIME_TURN: Duration = Duration::from_secs(5);
const CROSSFADE_MS: u32 = 350;

pub struct Collapsed {
    pub root: gtk::CenterBox,
    center: Stack,
    ring: Rc<Ring>,
    meter: Rc<Meter>,
    track: TrackCell,
    eyes: Rc<Eyes>,
    /// Eyes switched off in settings: the time stays.
    eyes_enabled: bool,
    mood: Cell<Option<Mood>>,
    turn: RefCell<Option<glib::SourceId>>,
}

impl Collapsed {
    pub fn new(eyes_enabled: bool) -> Rc<Self> {
        let time = Label::builder().css_classes(["clock"]).build();
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

        // Hidden pages are unmapped, so the eyes stop animating whenever the
        // time is showing.
        let center = Stack::builder()
            .transition_type(StackTransitionType::Crossfade)
            .transition_duration(CROSSFADE_MS)
            .build();
        center.add_named(&time, Some("time"));
        center.add_named(&face, Some("eyes"));
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
            ring,
            meter,
            track,
            eyes,
            eyes_enabled,
            // No update arrives when nothing plays at startup, so begin sleepy.
            mood: Cell::new(Some(Mood::Sleepy)),
            turn: RefCell::new(None),
        });
        collapsed.time_turn();
        let weak = Rc::downgrade(&collapsed);
        every_minute(move |now| {
            if let Ok(s) = now.format("%H:%M") {
                time.set_label(&s);
            }
            if let Some(c) = weak.upgrade() {
                c.time_turn();
            }
        });
        collapsed
    }

    pub fn set_track(self: &Rc<Self>, np: Option<&NowPlaying>) {
        self.ring.area.set_visible(np.is_some());
        self.meter.area.set_visible(np.is_some());
        self.track.replace(np.cloned());
        self.ring.refresh();
        self.meter.set_playing(np.is_some_and(|n| n.playing));

        let mood = if np.is_some_and(|n| n.playing) {
            Mood::Excited
        } else {
            Mood::Sleepy
        };
        if self.mood.replace(Some(mood)) != Some(mood) {
            self.eyes.set_mood(mood);
            self.time_turn(); // show the time once, then the new mood
        }
    }

    /// Shows the time now, then hands back to the eyes after a while.
    fn time_turn(self: &Rc<Self>) {
        self.show_turn("time", TIME_TURN);
    }

    fn show_turn(self: &Rc<Self>, page: &'static str, after: Duration) {
        if let Some(id) = self.turn.take() {
            id.remove();
        }
        self.center.set_visible_child_name(page);
        if !self.eyes_enabled {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(after, move || {
            let Some(c) = weak.upgrade() else { return };
            c.turn.take();
            if page == "eyes" {
                c.time_turn();
            } else if c.mood.get() == Some(Mood::Excited) {
                // Playing: the eyes get the same turn, then the time again.
                c.show_turn("eyes", TIME_TURN);
            } else {
                // Idle: the eyes stay until the next minute; no timer runs.
                c.center.set_visible_child_name("eyes");
            }
        });
        self.turn.replace(Some(id));
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
        let hint = Label::builder()
            .label("NOTHING PLAYING")
            .css_classes(["caps", "muted"])
            .build();

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["idle-card"])
            .width_request(crate::island::EXPANDED_IDLE.0 as i32)
            .height_request(crate::island::EXPANDED_IDLE.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&time);
        root.append(&date);
        root.append(&hint);

        every_minute(move |now: &glib::DateTime| {
            if let Ok(s) = now.format("%A  %d %B") {
                date.set_label(&s.to_uppercase());
            }
        });
        Self { root }
    }
}
