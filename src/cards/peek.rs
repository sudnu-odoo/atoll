//! Short-lived notification card (device connected, charger plugged).
//! The island shows it for a few seconds and folds back.

use std::cell::Cell;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, Align, DrawingArea, Label, Orientation};

use crate::palette::{self, Rgb};

const GAUGE: i32 = 36;

#[derive(Clone, PartialEq)]
pub enum Kind {
    Bluetooth {
        audio: bool,
    },
    Charging,
    Unplugged,
    Water,
    Claude,
    Command {
        ok: bool,
    },
    Meeting,
    Todo,
    /// With the sending app's (or sender's) picture, if known.
    Notification {
        icon: Option<gtk::gio::Icon>,
    },
}

#[derive(Clone)]
pub struct Peek {
    pub kind: Kind,
    pub title: String,
    pub subtitle: String,
    /// Battery level 0–100, drawn as the ring and (for power) a big number.
    pub percent: Option<f64>,
}

pub struct PeekCard {
    pub root: gtk::Box,
    gauge: DrawingArea,
    level: Rc<Cell<Option<(f64, Rgb)>>>,
    icon: gtk::Image,
    title: Label,
    subtitle: Label,
    percent: Label,
    badge: gtk::Image,
    /// Ring + symbol, or a notification's own picture.
    lead: gtk::Stack,
    picture: gtk::Image,
}

impl PeekCard {
    pub fn new() -> Self {
        let level: Rc<Cell<Option<(f64, Rgb)>>> = Rc::default();
        let gauge = DrawingArea::builder()
            .content_width(GAUGE)
            .content_height(GAUGE)
            .build();
        let l = level.clone();
        gauge.set_draw_func(move |_, cr, w, h| draw_gauge(cr, f64::from(w), f64::from(h), l.get()));
        let icon = gtk::Image::builder()
            .css_classes(["peek-icon"])
            .halign(Align::Center)
            .valign(Align::Center)
            .build();
        let ring = gtk::Overlay::builder()
            .child(&gauge)
            .valign(Align::Center)
            .build();
        ring.add_overlay(&icon);
        let picture = gtk::Image::builder().pixel_size(GAUGE).build();
        let frame = gtk::Box::builder()
            .css_classes(["notif-icon"])
            .overflow(gtk::Overflow::Hidden)
            .valign(Align::Center)
            .build();
        frame.append(&picture);
        let lead = gtk::Stack::builder().valign(Align::Center).build();
        lead.add_named(&ring, Some("ring"));
        lead.add_named(&frame, Some("picture"));

        // Capped widths: an ellipsizing label still *asks* for its full
        // text width, which would stretch the card past the notch outline.
        let title = Label::builder()
            .css_classes(["peek-title"])
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .build();
        let subtitle = Label::builder()
            .css_classes(["peek-subtitle"])
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(40)
            .build();
        let text = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .valign(Align::Center)
            .hexpand(true)
            .spacing(2)
            .build();
        text.append(&title);
        text.append(&subtitle);

        let percent = Label::builder()
            .css_classes(["peek-percent"])
            .valign(Align::Center)
            .build();
        let badge = gtk::Image::builder()
            .css_classes(["peek-badge"])
            .icon_name("atoll-headphones-symbolic")
            .valign(Align::Center)
            .build();

        let root = gtk::Box::builder()
            .css_classes(["peek"])
            .spacing(16)
            .width_request(crate::island::PEEK.0 as i32)
            .height_request(crate::island::PEEK.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&lead);
        root.append(&text);
        root.append(&percent);
        root.append(&badge);

        Self {
            root,
            gauge,
            level,
            icon,
            title,
            subtitle,
            percent,
            badge,
            lead,
            picture,
        }
    }

    pub fn show(&self, peek: &Peek) {
        let (icon, color) = match peek.kind {
            Kind::Bluetooth { .. } => ("atoll-bluetooth-symbolic", palette::lavender()),
            Kind::Charging => ("atoll-bolt-symbolic", palette::pink()),
            Kind::Unplugged => ("atoll-unplugged-symbolic", palette::MUTED),
            Kind::Water => ("atoll-drop-symbolic", palette::lavender()),
            Kind::Claude => ("atoll-claude-symbolic", palette::pink()),
            // Success is calm; failure takes the attention color.
            Kind::Command { ok: true } => ("atoll-check-symbolic", palette::lavender()),
            Kind::Command { ok: false } => ("atoll-close-symbolic", palette::pink()),
            Kind::Meeting => ("atoll-event-symbolic", palette::pink()),
            Kind::Todo => ("atoll-check-symbolic", palette::pink()),
            Kind::Notification { .. } => ("atoll-bell-symbolic", palette::lavender()),
        };
        self.icon.set_icon_name(Some(icon));
        // A notification with a known picture shows it instead of the ring.
        match &peek.kind {
            Kind::Notification { icon: Some(gicon) } => {
                self.picture.set_from_gicon(gicon);
                self.lead.set_visible_child_name("picture");
            }
            _ => self.lead.set_visible_child_name("ring"),
        }
        // No level (e.g. Claude): a full ring, so it reads as a badge
        // rather than an empty gauge.
        let fraction = peek.percent.map_or(1.0, |p| (p / 100.0).clamp(0.0, 1.0));
        self.level.set(Some((fraction, color)));
        self.gauge.queue_draw();
        // A peek is one line each: multi-line messages (code blocks, lists)
        // would otherwise stack down past the notch. The full text stays in
        // the notifications card.
        self.title.set_label(&one_line(&peek.title));
        self.subtitle.set_label(&one_line(&peek.subtitle));

        // Power peeks lead with the number; headphones get a badge instead.
        let audio = matches!(peek.kind, Kind::Bluetooth { audio: true });
        let power = matches!(peek.kind, Kind::Charging | Kind::Unplugged);
        self.badge.set_visible(audio);
        self.percent.set_visible(power && peek.percent.is_some());
        if let Some(p) = peek.percent {
            self.percent.set_label(&format!("{p:.0}%"));
        }
    }
}

fn draw_gauge(cr: &cairo::Context, w: f64, h: f64, level: Option<(f64, Rgb)>) {
    let (cx, cy) = (w / 2.0, h / 2.0);
    let r = w.min(h) / 2.0 - 2.0;
    cr.set_line_width(3.5);
    cr.set_line_cap(cairo::LineCap::Round);
    let (tr, tg, tb) = palette::BG_RAISED;
    cr.set_source_rgb(tr, tg, tb);
    cr.arc(cx, cy, r, 0.0, TAU);
    let _ = cr.stroke();

    if let Some((fraction, (r_, g, b))) = level.filter(|(f, _)| *f > 0.0) {
        cr.set_source_rgb(r_, g, b);
        cr.arc(cx, cy, r, -FRAC_PI_2, -FRAC_PI_2 + TAU * fraction);
        let _ = cr.stroke();
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn peeks_are_one_line() {
        assert_eq!(
            super::one_line("odev migrate\n\nFor example:\n  odev  x"),
            "odev migrate For example: odev x"
        );
    }
}
