//! Water card: today's total against the goal, an hourly chart, and quick
//! actions. Renders `water::View` and sends `water::Command`s.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, Align, DrawingArea, Label, Orientation};

use super::control_button;
use crate::palette;
use crate::water::{Command, View};

const LEFT_W: i32 = 236;

pub struct WaterCard {
    pub root: gtk::Box,
    amount: Label,
    goal: Label,
    status: Label,
    bar: DrawingArea,
    chart: DrawingArea,
    drink_text: Label,
    undo: gtk::Button,
    bell: gtk::Button,
    view: Rc<RefCell<Option<View>>>,
}

impl WaterCard {
    pub fn new(commands: async_channel::Sender<Command>) -> Self {
        // The island's shared top row (tabs, clock, settings) names the card
        // and shows the time; this row keeps only the card's own actions.
        let top = gtk::CenterBox::builder().css_classes(["card-top"]).build();

        let amount = Label::builder().css_classes(["water-amount"]).build();
        let goal = Label::builder()
            .css_classes(["water-goal"])
            .valign(Align::Baseline)
            .build();
        let figures = gtk::Box::builder().spacing(6).build();
        figures.append(&amount);
        figures.append(&goal);
        let status = Label::builder()
            .css_classes(["subtitle"])
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();

        let view: Rc<RefCell<Option<View>>> = Rc::default();
        let bar = DrawingArea::builder()
            .content_width(LEFT_W)
            .content_height(6)
            .build();
        let v = view.clone();
        bar.set_draw_func(move |_, cr, w, h| {
            let fraction = v
                .borrow()
                .as_ref()
                .map_or(0.0, |v| f64::from(v.total_ml) / f64::from(v.goal_ml.max(1)));
            draw_bar(cr, f64::from(w), f64::from(h), fraction);
        });

        let drink = control_button(&["pill", "primary-pill"], None);
        let drink_label = gtk::Box::builder().spacing(6).build();
        drink_label.append(&gtk::Image::from_icon_name("atoll-drop-symbolic"));
        let drink_text = Label::new(Some("Drink"));
        drink_label.append(&drink_text);
        drink.set_child(Some(&drink_label));
        let undo = control_button(&["round"], Some("atoll-undo-symbolic"));
        undo.set_tooltip_text(Some("Undo last drink"));
        let bell = control_button(&["round"], Some("atoll-bell-symbolic"));
        let send = |b: &gtk::Button, cmd: fn() -> Command| {
            let tx = commands.clone();
            b.connect_clicked(move |_| {
                let _ = tx.try_send(cmd());
            });
        };
        send(&drink, || Command::Drink);
        send(&undo, || Command::Undo);
        send(&bell, || Command::ToggleReminders);
        let actions = gtk::Box::builder()
            .spacing(10)
            .css_classes(["water-actions"])
            .build();
        actions.append(&drink);
        actions.append(&undo);
        actions.append(&bell);

        let left = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .width_request(LEFT_W)
            .spacing(6)
            .build();
        left.append(&figures);
        left.append(&status);
        left.append(&bar);
        left.append(&actions);

        let chart = DrawingArea::builder()
            .hexpand(true)
            .content_height(104)
            .valign(Align::End)
            .build();
        let v = view.clone();
        chart.set_draw_func(move |_, cr, w, h| {
            if let Some(v) = v.borrow().as_ref() {
                draw_chart(cr, f64::from(w), f64::from(h), v);
            }
        });

        let body = gtk::Box::builder().spacing(26).build();
        body.append(&left);
        body.append(&chart);

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["water-card"])
            .width_request(crate::island::EXPANDED_WATER.0 as i32)
            .height_request(crate::island::EXPANDED_WATER.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&top);
        root.append(&body);

        Self {
            root,
            amount,
            goal,
            status,
            bar,
            chart,
            drink_text,
            undo,
            bell,
            view,
        }
    }

    pub fn update(&self, v: &View) {
        let litres = |ml: u32| format!("{:.2}", f64::from(ml) / 1000.0);
        self.amount.set_label(&litres(v.total_ml));
        self.goal
            .set_label(&format!("/ {:.1} L", f64::from(v.goal_ml) / 1000.0));
        let pct = (100.0 * f64::from(v.total_ml) / f64::from(v.goal_ml.max(1))).round();
        let tail = if v.total_ml >= v.goal_ml {
            "Goal reached".to_owned()
        } else if !v.reminders {
            "Reminders off".to_owned()
        } else {
            match v.next_reminder_min {
                Some(0) => "Drink now".to_owned(),
                Some(m) if m < 60 => format!("Next in {m} min"),
                Some(m) => format!("Next in {}h {:02}m", m / 60, m % 60),
                None => "Done for today".to_owned(),
            }
        };
        self.status.set_label(&format!("Today · {pct}% · {tail}"));
        self.drink_text
            .set_label(&format!("Drink {} ml", v.glass_ml));
        self.undo.set_sensitive(v.can_undo);
        self.bell.set_icon_name(if v.reminders {
            "atoll-bell-symbolic"
        } else {
            "atoll-bell-off-symbolic"
        });
        self.bell.set_tooltip_text(Some(if v.reminders {
            "Reminders on"
        } else {
            "Reminders off"
        }));
        if v.reminders {
            self.bell.add_css_class("on");
        } else {
            self.bell.remove_css_class("on");
        }
        self.view.replace(Some(v.clone()));
        self.bar.queue_draw();
        self.chart.queue_draw();
    }
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::FRAC_PI_2;
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, 2.0 * FRAC_PI_2);
    cr.arc(x + r, y + r, r, 2.0 * FRAC_PI_2, 3.0 * FRAC_PI_2);
    cr.close_path();
}

/// Progress toward the goal: secondary fading into accent.
fn draw_bar(cr: &cairo::Context, w: f64, h: f64, fraction: f64) {
    let (r, g, b) = palette::bg_raised();
    cr.set_source_rgb(r, g, b);
    rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
    let _ = cr.fill();
    let filled = w * fraction.clamp(0.0, 1.0);
    if filled < 1.0 {
        return;
    }
    let grad = cairo::LinearGradient::new(0.0, 0.0, w, 0.0);
    let (lr, lg, lb) = palette::lavender();
    let (pr, pg, pb) = palette::pink();
    grad.add_color_stop_rgb(0.0, lr, lg, lb);
    grad.add_color_stop_rgb(1.0, pr, pg, pb);
    if cr.set_source(&grad).is_ok() {
        rounded_rect(cr, 0.0, 0.0, filled, h, h / 2.0);
        let _ = cr.fill();
    }
}

/// Hourly intake: one bar per active hour, the current hour in accent,
/// hour labels every three hours and a light guide at one glass.
fn draw_chart(cr: &cairo::Context, w: f64, h: f64, v: &View) {
    let label_h = 14.0;
    let plot_h = h - label_h;
    let n = v.per_hour.len().max(1) as f64;
    let slot = w / n;
    let bar_w = (slot * 0.55).clamp(4.0, 12.0);
    // Scale so one glass is a visible bar and big hours still fit.
    let max = v
        .per_hour
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(v.glass_ml * 2) as f64;

    // Guide line at one glass.
    let guide_y = plot_h - plot_h * f64::from(v.glass_ml) / max;
    let (mr, mg, mb) = palette::muted();
    cr.set_source_rgba(mr, mg, mb, 0.5);
    cr.set_line_width(1.0);
    cr.set_dash(&[2.0, 3.0], 0.0);
    cr.move_to(0.0, guide_y.round() + 0.5);
    cr.line_to(w, guide_y.round() + 0.5);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    cr.select_font_face(
        "Ubuntu",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Normal,
    );
    cr.set_font_size(10.0);
    for (i, ml) in v.per_hour.iter().enumerate() {
        let hour = v.first_hour + i as u32;
        let x = slot * i as f64 + (slot - bar_w) / 2.0;
        // Empty track, so the day's shape is visible even before drinking.
        let (tr, tg, tb) = palette::bg_raised();
        cr.set_source_rgba(tr, tg, tb, 0.55);
        rounded_rect(cr, x, 0.0, bar_w, plot_h, bar_w / 2.0);
        let _ = cr.fill();

        if *ml > 0 {
            let bh = (plot_h * f64::from(*ml) / max).max(bar_w);
            let (r, g, b) = if hour == v.current_hour {
                palette::pink()
            } else {
                palette::lavender()
            };
            cr.set_source_rgb(r, g, b);
            rounded_rect(cr, x, plot_h - bh, bar_w, bh, bar_w / 2.0);
            let _ = cr.fill();
        }

        if hour.is_multiple_of(3) {
            let text = format!("{hour:02}");
            if let Ok(ext) = cr.text_extents(&text) {
                cr.set_source_rgb(mr, mg, mb);
                cr.move_to(x + bar_w / 2.0 - ext.width() / 2.0, h - 2.0);
                let _ = cr.show_text(&text);
            }
        }
    }
}
