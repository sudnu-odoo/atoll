//! Small drawn widgets shared by several cards. Each one runs its own timer
//! only while it is on screen and has something to show, so hidden or idle
//! widgets cost nothing.

use std::cell::{Cell, RefCell};
use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{cairo, glib, Align, DrawingArea};

use crate::palette::{self, Rgb};
use crate::services::mpris::NowPlaying;

/// Shared "current track" cell that several widgets read when drawing.
pub type TrackCell = Rc<RefCell<Option<NowPlaying>>>;

/// The ring only needs to visibly creep forward, not tick every second.
const RING_REFRESH: Duration = Duration::from_secs(2);
/// ~12 fps: smooth enough for a 14px meter, a fraction of a full frame rate.
const METER_FRAME: Duration = Duration::from_millis(80);

fn set_source(cr: &cairo::Context, (r, g, b): Rgb) {
    cr.set_source_rgb(r, g, b);
}

/// Starts or stops `tick` so it runs only while `area` is mapped and
/// `wanted()` holds. Call `sync` again whenever `wanted()` may have changed.
struct Ticker {
    area: DrawingArea,
    every: Duration,
    id: RefCell<Option<glib::SourceId>>,
}

impl Ticker {
    fn new(area: &DrawingArea, every: Duration) -> Rc<Self> {
        Rc::new(Self {
            area: area.clone(),
            every,
            id: RefCell::new(None),
        })
    }

    fn sync(&self, wanted: bool) {
        let run = wanted && self.area.is_mapped();
        let running = self.id.borrow().is_some();
        if run == running {
            return;
        }
        if let Some(id) = self.id.take() {
            id.remove();
            return;
        }
        let area = self.area.downgrade();
        let id = glib::timeout_add_local(self.every, move || match area.upgrade() {
            Some(a) => {
                a.queue_draw();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        self.id.replace(Some(id));
    }
}

/// Track progress as a ring that fills clockwise.
pub struct Ring {
    pub area: DrawingArea,
    track: TrackCell,
    ticker: Rc<Ticker>,
}

impl Ring {
    pub fn new(track: TrackCell, size: i32) -> Rc<Self> {
        let area = DrawingArea::builder()
            .content_width(size)
            .content_height(size)
            .valign(Align::Center)
            .build();
        let t = track.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let fraction = t.borrow().as_ref().map_or(0.0, |np| {
                np.length.map_or(0.0, |len| {
                    np.position_now().as_secs_f64() / len.as_secs_f64().max(1.0)
                })
            });
            draw_ring(cr, f64::from(w), f64::from(h), fraction);
        });
        let ring = Rc::new(Self {
            ticker: Ticker::new(&area, RING_REFRESH),
            area,
            track,
        });
        let weak = Rc::downgrade(&ring);
        ring.area.connect_map(move |_| {
            if let Some(r) = weak.upgrade() {
                r.refresh();
            }
        });
        let weak = Rc::downgrade(&ring);
        ring.area.connect_unmap(move |_| {
            if let Some(r) = weak.upgrade() {
                r.ticker.sync(false);
            }
        });
        ring
    }

    /// Call after the shared track changed.
    pub fn refresh(&self) {
        let playing = self.track.borrow().as_ref().is_some_and(|np| np.playing);
        self.ticker.sync(playing);
        self.area.queue_draw();
    }
}

fn draw_ring(cr: &cairo::Context, w: f64, h: f64, fraction: f64) {
    let (cx, cy) = (w / 2.0, h / 2.0);
    let r = w.min(h) / 2.0 - 1.5;
    cr.set_line_width(2.5);
    cr.set_line_cap(cairo::LineCap::Round);

    set_source(cr, palette::BG_RAISED);
    cr.arc(cx, cy, r, 0.0, TAU);
    let _ = cr.stroke();

    if fraction > 0.0 {
        set_source(cr, palette::pink());
        let start = -FRAC_PI_2;
        cr.arc(cx, cy, r, start, start + TAU * fraction.min(1.0));
        let _ = cr.stroke();
    }
}

/// A small level meter that dances while music plays. MPRIS exposes no
/// audio levels, so the motion is a pleasant fake, not a real visualizer.
pub struct Meter {
    pub area: DrawingArea,
    playing: Rc<Cell<bool>>,
    ticker: Rc<Ticker>,
}

impl Meter {
    pub fn new() -> Rc<Self> {
        let area = DrawingArea::builder()
            .content_width(16)
            .content_height(14)
            .valign(Align::Center)
            .build();
        let playing = Rc::new(Cell::new(false));
        let p = playing.clone();
        area.set_draw_func(move |_, cr, w, h| {
            draw_meter(cr, f64::from(w), f64::from(h), p.get());
        });
        let meter = Rc::new(Self {
            ticker: Ticker::new(&area, METER_FRAME),
            area,
            playing,
        });
        let weak = Rc::downgrade(&meter);
        meter.area.connect_map(move |_| {
            if let Some(m) = weak.upgrade() {
                m.ticker.sync(m.playing.get());
            }
        });
        let weak = Rc::downgrade(&meter);
        meter.area.connect_unmap(move |_| {
            if let Some(m) = weak.upgrade() {
                m.ticker.sync(false);
            }
        });
        meter
    }

    pub fn set_playing(&self, playing: bool) {
        self.playing.set(playing);
        self.ticker.sync(playing);
        self.area.queue_draw();
    }
}

fn draw_meter(cr: &cairo::Context, w: f64, h: f64, playing: bool) {
    // Different speeds and phases per bar so the motion never looks looped.
    const BARS: [(f64, f64); 4] = [(5.1, 0.0), (7.3, 1.7), (6.2, 3.1), (8.4, 4.4)];
    let t = glib::monotonic_time() as f64 / 1_000_000.0;
    let n = BARS.len() as f64;
    let gap = 2.0;
    let bw = (w - gap * (n - 1.0)) / n;
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_width(bw);
    // Paused: bars collapse to dots, like a meter at rest.
    set_source(
        cr,
        if playing {
            palette::lavender()
        } else {
            palette::MUTED
        },
    );
    for (i, (speed, phase)) in BARS.iter().enumerate() {
        let x = bw / 2.0 + i as f64 * (bw + gap);
        let level = 0.3 + 0.7 * ((t * speed + phase).sin() * 0.5 + 0.5);
        let len = if playing { (h - bw) * level } else { 0.0 };
        let y0 = (h - len) / 2.0;
        cr.move_to(x, y0);
        cr.line_to(x, y0 + len);
    }
    let _ = cr.stroke();
}

/// Album art, cropped to a rounded square of exactly `size` px. A
/// `gtk::Picture` would instead request the image's natural size and
/// stretch the layout (YouTube covers are 16:9).
pub struct Cover {
    pub area: DrawingArea,
    image: Rc<RefCell<Option<cairo::ImageSurface>>>,
}

impl Cover {
    pub fn new(size: i32, radius: f64) -> Self {
        let area = DrawingArea::builder()
            .content_width(size)
            .content_height(size)
            .halign(Align::Start)
            .valign(Align::Center)
            .build();
        let image: Rc<RefCell<Option<cairo::ImageSurface>>> = Rc::default();
        let img = image.clone();
        area.set_draw_func(move |_, cr, w, h| {
            draw_cover(
                cr,
                f64::from(w),
                f64::from(h),
                radius,
                img.borrow().as_ref(),
            );
        });
        Self { area, image }
    }

    pub fn set(&self, texture: Option<&gtk::gdk::Texture>) {
        self.image.replace(texture.and_then(to_surface));
        self.area.queue_draw();
    }
}

/// Converts once per cover, so drawing is just a scaled blit.
fn to_surface(texture: &gtk::gdk::Texture) -> Option<cairo::ImageSurface> {
    let (w, h) = (texture.width(), texture.height());
    let stride = cairo::Format::ARgb32
        .stride_for_width(w.unsigned_abs())
        .ok()?;
    let mut data = vec![0u8; stride as usize * h as usize];
    // GDK's default download format is premultiplied BGRA in native order,
    // which is exactly cairo's ARGB32.
    texture.download(&mut data, stride as usize);
    cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, stride).ok()
}

fn draw_cover(
    cr: &cairo::Context,
    w: f64,
    h: f64,
    radius: f64,
    image: Option<&cairo::ImageSurface>,
) {
    let r = radius.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(w - r, r, r, -FRAC_PI_2, 0.0);
    cr.arc(w - r, h - r, r, 0.0, FRAC_PI_2);
    cr.arc(r, h - r, r, FRAC_PI_2, 2.0 * FRAC_PI_2);
    cr.arc(r, r, r, 2.0 * FRAC_PI_2, 3.0 * FRAC_PI_2);
    cr.close_path();
    cr.clip();

    let Some(img) = image else {
        set_source(cr, palette::BG_RAISED);
        let _ = cr.paint();
        return;
    };
    // Cover: scale to fill, crop the overflow evenly.
    let (iw, ih) = (f64::from(img.width()), f64::from(img.height()));
    let scale = (w / iw).max(h / ih);
    cr.translate((w - iw * scale) / 2.0, (h - ih * scale) / 2.0);
    cr.scale(scale, scale);
    if cr.set_source_surface(img, 0.0, 0.0).is_ok() {
        cr.source().set_filter(cairo::Filter::Good);
        let _ = cr.paint();
    }
}

/// Atoll's character: two eyes that take turns with the clock. Sleepy when
/// nothing plays (half-closed, blinking, sometimes dozing off); excited
/// while music plays (happy arcs that hop to the music). They rest still
/// between motions (no frames drawn); frames run only during those short
/// motions. Both eyes share one state, so they always move together.
pub struct Eyes {
    pub left: DrawingArea,
    pub right: DrawingArea,
    mood: Rc<Cell<Mood>>,
    /// Sleepy: openness (0 = closed, 1 = wide open). Excited: hop offset
    /// in px (negative = up).
    value: Rc<Cell<f64>>,
    motion: RefCell<Option<EyeMotion>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    next: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mood {
    Sleepy,
    Excited,
}

impl Mood {
    fn rest(self) -> f64 {
        match self {
            Mood::Sleepy => EYES_REST,
            Mood::Excited => 0.0,
        }
    }
}

/// Keyframes as (seconds from start, value), eased between.
struct EyeMotion {
    start_us: i64,
    keys: &'static [(f64, f64)],
}

/// Resting openness: heavy-lidded.
const EYES_REST: f64 = 0.42;
const BLINK: &[(f64, f64)] = &[(0.0, EYES_REST), (0.09, 0.0), (0.22, EYES_REST)];
/// Lids sink slowly, stay shut a moment, then snap half open again.
const DOZE: &[(f64, f64)] = &[
    (0.0, EYES_REST),
    (1.4, 0.06),
    (3.2, 0.06),
    (3.45, EYES_REST + 0.12),
    (3.9, EYES_REST),
];
/// A happy double hop, like bopping to the beat.
const HOP: &[(f64, f64)] = &[
    (0.0, 0.0),
    (0.12, -3.5),
    (0.24, 0.0),
    (0.34, -1.8),
    (0.44, 0.0),
];
/// Milliseconds between motions; random so it never feels mechanical.
/// Excited eyes move more often than sleepy ones.
const SLEEPY_PAUSE_MS: (i32, i32) = (4000, 9000);
const EXCITED_PAUSE_MS: (i32, i32) = (1500, 3500);
/// One in this many motions is a doze instead of a blink.
const DOZE_ODDS: i32 = 4;

impl Eyes {
    pub fn new() -> Rc<Self> {
        let mood = Rc::new(Cell::new(Mood::Sleepy));
        let value = Rc::new(Cell::new(EYES_REST));
        let eye = || {
            let area = DrawingArea::builder()
                .content_width((EYE_W + 2.0 * GLOW) as i32)
                .content_height((EYE_H + 2.0 * GLOW) as i32)
                .valign(Align::Center)
                .build();
            let (m, v) = (mood.clone(), value.clone());
            area.set_draw_func(move |_, cr, _, _| match m.get() {
                Mood::Sleepy => draw_eye(cr, v.get()),
                Mood::Excited => draw_happy_eye(cr, v.get()),
            });
            area
        };
        let (left, right) = (eye(), eye());
        let eyes = Rc::new(Self {
            left,
            right,
            mood,
            value,
            motion: RefCell::new(None),
            tick: RefCell::new(None),
            next: RefCell::new(None),
        });
        // Live only while on screen: hidden eyes schedule nothing. The left
        // eye stands in for both; they're always shown and hidden together.
        let weak = Rc::downgrade(&eyes);
        eyes.left.connect_map(move |_| {
            if let Some(e) = weak.upgrade() {
                e.schedule();
            }
        });
        let weak = Rc::downgrade(&eyes);
        eyes.left.connect_unmap(move |_| {
            if let Some(e) = weak.upgrade() {
                e.stop();
            }
        });
        eyes
    }

    pub fn set_mood(self: &Rc<Self>, mood: Mood) {
        if self.mood.replace(mood) == mood {
            return;
        }
        self.stop();
        self.left.queue_draw();
        self.right.queue_draw();
        if self.left.is_mapped() {
            self.schedule();
        }
    }

    fn schedule(self: &Rc<Self>) {
        if let Some(id) = self.next.take() {
            id.remove();
        }
        let (lo, hi) = match self.mood.get() {
            Mood::Sleepy => SLEEPY_PAUSE_MS,
            Mood::Excited => EXCITED_PAUSE_MS,
        };
        let ms = glib::random_int_range(lo, hi + 1).unsigned_abs();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(ms.into()), move || {
            if let Some(e) = weak.upgrade() {
                e.next.take();
                let keys = match e.mood.get() {
                    Mood::Excited => HOP,
                    Mood::Sleepy if glib::random_int_range(0, DOZE_ODDS) == 0 => DOZE,
                    Mood::Sleepy => BLINK,
                };
                e.play(keys);
            }
        });
        self.next.replace(Some(id));
    }

    fn play(self: &Rc<Self>, keys: &'static [(f64, f64)]) {
        let Some(clock) = self.left.frame_clock() else {
            return;
        };
        self.motion.replace(Some(EyeMotion {
            start_us: clock.frame_time(),
            keys,
        }));
        let weak = Rc::downgrade(self);
        let id = self.left.add_tick_callback(move |_, clock| {
            let Some(e) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            e.step(clock.frame_time())
        });
        self.tick.replace(Some(id));
    }

    fn step(self: &Rc<Self>, now_us: i64) -> glib::ControlFlow {
        let Some((t, keys)) = self
            .motion
            .borrow()
            .as_ref()
            .map(|m| ((now_us - m.start_us) as f64 / 1e6, m.keys))
        else {
            return glib::ControlFlow::Break;
        };
        let (value, done) = sample(keys, t);
        self.value.set(value);
        self.left.queue_draw();
        self.right.queue_draw();
        if !done {
            return glib::ControlFlow::Continue;
        }
        self.motion.replace(None);
        self.tick.take();
        self.schedule();
        glib::ControlFlow::Break
    }

    fn stop(&self) {
        if let Some(id) = self.next.take() {
            id.remove();
        }
        if let Some(id) = self.tick.take() {
            id.remove();
        }
        self.motion.replace(None);
        self.value.set(self.mood.get().rest());
    }
}

/// Smoothstep between keyframes; returns (value, finished).
fn sample(keys: &[(f64, f64)], t: f64) -> (f64, bool) {
    for pair in keys.windows(2) {
        let ((t0, v0), (t1, v1)) = (pair[0], pair[1]);
        if t <= t1 {
            let x = ((t - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0);
            let s = x * x * (3.0 - 2.0 * x);
            return (v0 + (v1 - v0) * s, false);
        }
    }
    (keys.last().map_or(EYES_REST, |k| k.1), true)
}

/// Eye size (px): wide and rounded reads as cute; 22px tall is as much as
/// the 32px notch allows with room for the glow.
const EYE_W: f64 = 16.0;
const EYE_H: f64 = 22.0;
/// Room around each eye for its glow.
const GLOW: f64 = 4.0;

fn draw_eye(cr: &cairo::Context, openness: f64) {
    let (lr, lg, lb) = palette::lavender();
    let (x, top) = (GLOW, GLOW);
    let bottom = top + EYE_H;
    let visible = (EYE_H * openness).max(2.0);
    let lid = bottom - visible;
    let (cx, cy) = (x + EYE_W / 2.0, top + EYE_H / 2.0);

    // Soft halo, brighter as the eye opens, so it reads on the black notch.
    let glow =
        cairo::RadialGradient::new(cx, cy + EYE_H * 0.2, 0.0, cx, cy, EYE_W.max(EYE_H) * 0.75);
    glow.add_color_stop_rgba(0.0, lr, lg, lb, 0.45 * openness.max(0.25));
    glow.add_color_stop_rgba(1.0, lr, lg, lb, 0.0);
    if cr.set_source(&glow).is_ok() {
        let _ = cr.paint();
    }

    // The lid comes down from the top: show only the lower part of the eye,
    // with a flat top edge.
    cr.save().ok();
    cr.rectangle(x - 1.0, lid, EYE_W + 2.0, visible + 1.0);
    cr.clip();
    cr.translate(cx, cy);
    cr.scale(EYE_W / 2.0, EYE_H / 2.0);
    cr.arc(0.0, 0.0, 1.0, 0.0, TAU);
    cr.set_source_rgb(lr, lg, lb);
    // Fill inside save/restore: restoring first would drop the lid clip.
    let _ = cr.fill();
    cr.restore().ok();

    // Lid edge: a thin line that makes half-closed eyes read as sleepy eyes
    // rather than two blobs.
    if openness > 0.05 {
        let half = lid_half_width(visible);
        set_source(cr, palette::MUTED);
        cr.set_line_width(1.5);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.move_to(cx - half, lid);
        cr.line_to(cx + half, lid);
        let _ = cr.stroke();
    }
}

/// Excited: a happy closed-eye arc (∩), pink, lifted by `hop` px.
fn draw_happy_eye(cr: &cairo::Context, hop: f64) {
    let (pr, pg, pb) = palette::pink();
    let cx = GLOW + EYE_W / 2.0;
    let cy = GLOW + EYE_H * 0.62 + hop;
    let (rx, ry) = (EYE_W / 2.0 - 1.5, EYE_H * 0.36);

    let glow = cairo::RadialGradient::new(cx, cy, 0.0, cx, cy, EYE_W.max(EYE_H) * 0.75);
    glow.add_color_stop_rgba(0.0, pr, pg, pb, 0.35);
    glow.add_color_stop_rgba(1.0, pr, pg, pb, 0.0);
    if cr.set_source(&glow).is_ok() {
        let _ = cr.paint();
    }

    // Upper half of an ellipse; scale only the path so the stroke stays round.
    cr.save().ok();
    cr.translate(cx, cy);
    cr.scale(rx, ry);
    cr.arc(0.0, 0.0, 1.0, std::f64::consts::PI, TAU);
    cr.restore().ok();
    cr.set_source_rgb(pr, pg, pb);
    cr.set_line_width(4.0);
    cr.set_line_cap(cairo::LineCap::Round);
    let _ = cr.stroke();
}

/// Half-width of the eye's ellipse at the lid line, so the lid edge spans
/// exactly the visible part of the eye.
fn lid_half_width(visible: f64) -> f64 {
    let ry = EYE_H / 2.0;
    let dy = (ry - visible).abs().min(ry);
    (EYE_W / 2.0) * (1.0 - (dy / ry).powi(2)).max(0.0).sqrt()
}
