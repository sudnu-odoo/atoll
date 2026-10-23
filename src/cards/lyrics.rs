//! Three-row lyrics view: previous, current, next, with the current line at
//! the exact vertical center. Long lines wrap (up to two lines each). When
//! the song moves on, the column scrolls up smoothly: the row leaving the
//! top fades out and the row arriving at the bottom fades in. A translation,
//! when there is one, sits under its line in a smaller, dimmer face.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{glib, Align, Label, Orientation, PolicyType};

/// Visible height; fits three rows even when all of them wrap.
const VIEW_H: i32 = 150;
const ROW_GAP: i32 = 10;
const SLIDE: Duration = Duration::from_millis(300);
/// Rows held, as offsets from the line being sung. The first is the row
/// leaving (faded out once settled); the others are what you see.
const ROWS: [isize; 4] = [-2, -1, 0, 1];
const CURRENT: usize = 2;

pub struct LyricsView {
    pub root: gtk::ScrolledWindow,
    labels: [Label; 4],
    shown: Cell<Option<isize>>,
    anim: RefCell<Option<Slide>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
}

struct Slide {
    from: f64,
    start_us: i64,
    /// Instant jumps reuse the same path so the target is computed after
    /// GTK has laid out the new text.
    duration: Duration,
}

impl LyricsView {
    pub fn new() -> Rc<Self> {
        let labels: [Label; 4] = std::array::from_fn(|i| {
            Label::builder()
                .css_classes([if i == CURRENT {
                    "lyric-current"
                } else {
                    "lyric"
                }])
                .use_markup(true)
                .halign(Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                // Beyond two lines a row would crowd the others out of view.
                .lines(2)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build()
        });
        // Half-height spacers above and below let any row, even the first
        // or last, be scrolled to the center.
        let inner = gtk::Box::new(Orientation::Vertical, ROW_GAP);
        inner.append(&spacer());
        for l in &labels {
            inner.append(l);
        }
        inner.append(&spacer());

        // A scrolled window clips and offsets its child; it is driven only by
        // code (no target, no scrollbar), never by the user.
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::External)
            .min_content_height(VIEW_H)
            .max_content_height(VIEW_H)
            .propagate_natural_height(false)
            .valign(Align::Center)
            .hexpand(true)
            .can_target(false)
            .child(&inner)
            .build();
        Rc::new(Self {
            root,
            labels,
            shown: Cell::new(None),
            anim: RefCell::new(None),
            tick: RefCell::new(None),
        })
    }

    /// With translations the text gets smaller and each row may take more
    /// lines, so three rows still fit.
    pub fn set_translated(self: &Rc<Self>, on: bool) {
        if self.root.has_css_class("translated") == on {
            return;
        }
        if on {
            self.root.add_css_class("translated");
        } else {
            self.root.remove_css_class("translated");
        }
        for l in &self.labels {
            l.set_lines(if on { 4 } else { 2 });
        }
        self.shown.set(None); // redo the layout at the new size
    }

    /// Shows a message on the center row (loading, no lyrics).
    pub fn message(self: &Rc<Self>, text: &str) {
        let text = markup(text, None);
        if self.shown.replace(None).is_none() && self.labels[CURRENT].label() == text {
            return;
        }
        for (i, l) in self.labels.iter().enumerate() {
            set_if_changed(l, if i == CURRENT { &text } else { "" });
        }
        self.jump();
    }

    /// `index` is the line being sung (-1 before the first line); `line(i)`
    /// returns the text of line `i` and its translation, if it exists.
    pub fn show<'a>(
        self: &Rc<Self>,
        index: isize,
        line: impl Fn(isize) -> Option<(&'a str, Option<&'a str>)>,
    ) {
        let previous = self.shown.replace(Some(index));
        if previous == Some(index) {
            return;
        }
        // Dropping the top row moves everything up by its height; compensate
        // so nothing visibly jumps, then animate to the new center.
        let dropped = self.row_height(0) + f64::from(ROW_GAP);
        for (l, offset) in self.labels.iter().zip(ROWS) {
            let text = line(index + offset).map_or(String::new(), |(t, tr)| markup(t, tr));
            set_if_changed(l, &text);
        }
        // Only a step to the next line slides; seeks and jumps snap.
        if previous == Some(index - 1) {
            let adj = self.root.vadjustment();
            self.start(adj.value() - dropped, SLIDE);
        } else {
            self.jump();
        }
    }

    fn jump(self: &Rc<Self>) {
        self.start(0.0, Duration::ZERO);
    }

    fn start(self: &Rc<Self>, from: f64, duration: Duration) {
        let Some(clock) = self.root.frame_clock() else {
            return;
        };
        self.anim.replace(Some(Slide {
            from,
            start_us: clock.frame_time(),
            duration,
        }));
        if self.tick.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = self.root.add_tick_callback(move |_, clock| {
            let Some(view) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            view.step(clock.frame_time())
        });
        self.tick.replace(Some(id));
    }

    /// Runs once per frame while sliding; nothing runs once settled.
    fn step(&self, now_us: i64) -> glib::ControlFlow {
        let Some((from, t)) = self.anim.borrow().as_ref().map(|a| {
            let total = a.duration.as_micros().max(1) as f64;
            (
                a.from,
                ((now_us - a.start_us) as f64 / total).clamp(0.0, 1.0),
            )
        }) else {
            return glib::ControlFlow::Break;
        };
        let eased = 1.0 - (1.0 - t).powi(3);
        let target = self.center_target();
        let adj = self.root.vadjustment();
        adj.set_value(if t >= 1.0 {
            target
        } else {
            from + (target - from) * eased
        });

        // The leaving row fades out, the arriving row fades in.
        self.labels[0].set_opacity(1.0 - eased);
        self.labels[3].set_opacity(eased);
        for l in &self.labels[1..3] {
            l.set_opacity(1.0);
        }

        if t < 1.0 {
            return glib::ControlFlow::Continue;
        }
        self.anim.replace(None);
        self.tick.take();
        glib::ControlFlow::Break
    }

    /// Scroll offset that puts the middle of the current row at the middle
    /// of the view.
    fn center_target(&self) -> f64 {
        let gap = f64::from(ROW_GAP);
        let above: f64 = (0..CURRENT).map(|i| self.row_height(i) + gap).sum();
        let spacer = f64::from(VIEW_H) / 2.0 + gap;
        spacer + above + self.row_height(CURRENT) / 2.0 - f64::from(VIEW_H) / 2.0
    }

    /// Height the row needs at the view's width, valid even before GTK has
    /// laid out freshly set text.
    fn row_height(&self, i: usize) -> f64 {
        let width = self.root.width().max(1);
        let (_, natural, _, _) = self.labels[i].measure(Orientation::Vertical, width);
        f64::from(natural)
    }
}

fn spacer() -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 0);
    b.set_height_request(VIEW_H / 2);
    b
}

fn markup(text: &str, translation: Option<&str>) -> String {
    let mut m = glib::markup_escape_text(text).to_string();
    if let Some(tr) = translation {
        m.push_str(&format!(
            "\n<span size=\"72%\" weight=\"normal\" alpha=\"68%\">{}</span>",
            glib::markup_escape_text(tr)
        ));
    }
    m
}

/// `set_label` always relayouts; skip it when nothing changed.
fn set_if_changed(label: &Label, text: &str) {
    if label.label() != text {
        label.set_label(text);
    }
}
