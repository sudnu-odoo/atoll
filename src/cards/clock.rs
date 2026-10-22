//! The collapsed notch (time, plus a small now-playing hint) and the
//! expanded card shown when nothing is playing.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{glib, Align, Label, Orientation};

use super::every_minute;
use super::widgets::{Meter, Ring, TrackCell};
use crate::services::mpris::NowPlaying;

pub struct Collapsed {
    pub root: gtk::CenterBox,
    ring: Rc<Ring>,
    meter: Rc<Meter>,
    track: TrackCell,
}

impl Collapsed {
    pub fn new() -> Self {
        let time = Label::builder().css_classes(["clock"]).build();
        let track = TrackCell::default();
        let ring = Ring::new(track.clone(), 16);
        let meter = Meter::new();
        ring.area.set_visible(false);
        meter.area.set_visible(false);

        let root = gtk::CenterBox::builder()
            .css_classes(["collapsed"])
            .width_request(crate::island::COLLAPSED.0 as i32)
            .height_request(crate::island::COLLAPSED.1 as i32)
            .start_widget(&ring.area)
            .center_widget(&time)
            .end_widget(&meter.area)
            .build();

        every_minute(move |now| {
            if let Ok(s) = now.format("%H:%M") {
                time.set_label(&s);
            }
        });
        Self {
            root,
            ring,
            meter,
            track,
        }
    }

    pub fn set_track(&self, np: Option<&NowPlaying>) {
        self.ring.area.set_visible(np.is_some());
        self.meter.area.set_visible(np.is_some());
        self.track.replace(np.cloned());
        self.ring.refresh();
        self.meter.set_playing(np.is_some_and(|n| n.playing));
    }
}

pub struct Idle {
    pub root: gtk::Box,
}

impl Idle {
    pub fn new() -> Self {
        let time = Label::builder().css_classes(["big-clock"]).build();
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
            if let Ok(s) = now.format("%H:%M") {
                time.set_label(&s);
            }
            if let Ok(s) = now.format("%A  %d %B") {
                date.set_label(&s.to_uppercase());
            }
        });
        Self { root }
    }
}
