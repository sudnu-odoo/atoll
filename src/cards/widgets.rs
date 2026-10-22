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
        set_source(cr, palette::PINK);
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
            palette::LAVENDER
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
