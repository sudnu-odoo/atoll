//! Calendar card, like GNOME's: a month grid (‹ › to change month, a dot on
//! days with events, today highlighted) and the selected day's events on
//! the right, with Join buttons for video calls.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use gtk::prelude::*;
use gtk::{gio, glib, Align, Label, Orientation};

use super::{control_button, live_clock};
use crate::services::calendar::Meeting;

const WEEKDAYS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];

pub struct CalendarCard {
    pub root: gtk::Box,
    month_label: Label,
    grid: gtk::Grid,
    day_title: Label,
    day_list: gtk::Box,
    /// Month on display, the selected day, and that month's events by day.
    shown: Cell<(i32, u32)>,
    selected: Cell<NaiveDate>,
    by_day: RefCell<HashMap<NaiveDate, Vec<Meeting>>>,
    request_month: Box<dyn Fn(i32, u32)>,
}

impl CalendarCard {
    pub fn new(request_month: impl Fn(i32, u32) + 'static) -> Rc<Self> {
        // Top row sits under the GNOME bar (no clicks there): labels only.
        let heading = gtk::Box::builder().spacing(6).build();
        heading.append(
            &gtk::Image::builder()
                .icon_name("atoll-event-symbolic")
                .css_classes(["todo-icon"])
                .build(),
        );
        heading.append(
            &Label::builder()
                .label("CALENDAR")
                .css_classes(["caps", "lavender"])
                .build(),
        );
        let time = Label::builder().css_classes(["clock"]).build();
        live_clock(&time, "%H:%M:%S");
        let top = gtk::CenterBox::builder()
            .css_classes(["card-top"])
            .start_widget(&heading)
            .center_widget(&time)
            .build();

        let prev = control_button(&["cal-nav"], Some("atoll-left-symbolic"));
        let next = control_button(&["cal-nav"], Some("atoll-right-symbolic"));
        let month_label = Label::builder().css_classes(["cal-month"]).build();
        let nav = gtk::CenterBox::builder()
            .start_widget(&prev)
            .center_widget(&month_label)
            .end_widget(&next)
            .build();
        let grid = gtk::Grid::builder()
            .row_homogeneous(true)
            .column_homogeneous(true)
            .css_classes(["cal-grid"])
            .build();
        let month = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .width_request(238)
            .build();
        month.append(&nav);
        month.append(&grid);

        let day_title = Label::builder()
            .css_classes(["cal-day-title"])
            .xalign(0.0)
            .build();
        let day_list = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(6)
            .build();
        // Busy days scroll; scrolling here doesn't flip cards.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&day_list)
            .build();
        let day = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .build();
        day.append(&day_title);
        day.append(&scroller);

        let body = gtk::Box::builder()
            .spacing(22)
            .css_classes(["cal-body"])
            .build();
        body.append(&month);
        body.append(&day);

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["water-card"])
            .width_request(crate::island::EXPANDED_CALENDAR.0 as i32)
            .height_request(crate::island::EXPANDED_CALENDAR.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&top);
        root.append(&body);

        let today = Local::now().date_naive();
        let card = Rc::new(Self {
            root,
            month_label,
            grid,
            day_title,
            day_list,
            shown: Cell::new((today.year(), today.month())),
            selected: Cell::new(today),
            by_day: RefCell::default(),
            request_month: Box::new(request_month),
        });
        let weak = Rc::downgrade(&card);
        prev.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.step_month(-1);
            }
        });
        let weak = Rc::downgrade(&card);
        next.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.step_month(1);
            }
        });
        card.render_grid();
        card.render_day();
        (card.request_month)(today.year(), today.month());
        card
    }

    /// A month's events arrived from the service.
    pub fn set_month(self: &Rc<Self>, year: i32, month: u32, events: &[Meeting]) {
        if self.shown.get() != (year, month) {
            return; // the user has moved on to another month
        }
        let mut by_day: HashMap<NaiveDate, Vec<Meeting>> = HashMap::new();
        for e in events {
            for d in days_covered(e) {
                by_day.entry(d).or_default().push(e.clone());
            }
        }
        for list in by_day.values_mut() {
            // All-day first, then by start time.
            list.sort_by_key(|m| (!m.all_day, m.start));
        }
        self.by_day.replace(by_day);
        self.render_grid();
        self.render_day();
    }

    /// Called once a minute: keeps "in N min" fresh and follows midnight.
    pub fn tick(self: &Rc<Self>) {
        self.render_day();
    }

    fn step_month(self: &Rc<Self>, delta: i32) {
        let (y, m) = self.shown.get();
        let index = y * 12 + m as i32 - 1 + delta;
        let (y, m) = (index.div_euclid(12), index.rem_euclid(12) as u32 + 1);
        self.shown.set((y, m));
        self.by_day.replace(HashMap::new());
        // Selecting today when coming back to this month, else the 1st.
        let today = Local::now().date_naive();
        self.selected
            .set(if (today.year(), today.month()) == (y, m) {
                today
            } else {
                NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(today)
            });
        self.render_grid();
        self.render_day();
        (self.request_month)(y, m);
    }

    fn render_grid(self: &Rc<Self>) {
        let (y, m) = self.shown.get();
        let Some(first) = NaiveDate::from_ymd_opt(y, m, 1) else {
            return;
        };
        self.month_label
            .set_label(&first.format("%B %Y").to_string());
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        for (col, name) in WEEKDAYS.iter().enumerate() {
            self.grid.attach(
                &Label::builder()
                    .label(*name)
                    .css_classes(["cal-weekday"])
                    .build(),
                col as i32,
                0,
                1,
                1,
            );
        }
        // Monday-first grid starting on the Monday on/before the 1st.
        let offset = i64::from(first.weekday().num_days_from_monday());
        let start = first - chrono::Duration::days(offset);
        let today = Local::now().date_naive();
        let selected = self.selected.get();
        let by_day = self.by_day.borrow();
        for i in 0..42 {
            let date = start + chrono::Duration::days(i);
            let mut classes = vec!["cal-day"];
            if date.month() != m {
                classes.push("other-month");
            }
            if date == today {
                classes.push("today");
            }
            if date == selected {
                classes.push("selected");
            }
            if by_day.contains_key(&date) {
                classes.push("has-events");
            }
            // Number with a dot slot under it; the slot is always there so
            // numbers line up whether or not a day has events.
            let content = gtk::Box::builder()
                .orientation(Orientation::Vertical)
                .halign(Align::Center)
                .valign(Align::Center)
                .build();
            content.append(&Label::new(Some(&date.day().to_string())));
            content.append(
                &gtk::Box::builder()
                    .css_classes(["cal-dot"])
                    .halign(Align::Center)
                    .build(),
            );
            let b = gtk::Button::builder()
                .child(&content)
                .css_classes(classes)
                .focus_on_click(false)
                .can_focus(false)
                .build();
            b.set_cursor_from_name(Some("pointer"));
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    if date.month() != c.shown.get().1 {
                        // A neighbouring month's day: go there.
                        c.step_month(if date < c.selected.get() { -1 } else { 1 });
                    }
                    c.selected.set(date);
                    c.render_grid();
                    c.render_day();
                }
            });
            self.grid
                .attach(&b, (i % 7) as i32, (i / 7 + 1) as i32, 1, 1);
        }
    }

    fn render_day(&self) {
        let date = self.selected.get();
        let today = Local::now().date_naive();
        let title = match (date - today).num_days() {
            0 => format!("Today · {}", date.format("%a %d %b")),
            1 => format!("Tomorrow · {}", date.format("%a %d %b")),
            -1 => format!("Yesterday · {}", date.format("%a %d %b")),
            _ => date.format("%A %d %B").to_string(),
        };
        self.day_title.set_label(&title);
        while let Some(child) = self.day_list.first_child() {
            self.day_list.remove(&child);
        }
        let by_day = self.by_day.borrow();
        let Some(events) = by_day.get(&date).filter(|e| !e.is_empty()) else {
            self.day_list.append(
                &Label::builder()
                    .label("No events")
                    .css_classes(["subtitle"])
                    .xalign(0.0)
                    .build(),
            );
            return;
        };
        let now = glib::DateTime::now_local().map_or(0, |t| t.to_unix());
        // The next timed event still ahead gets an "in N min" tag.
        let next_id = events
            .iter()
            .filter(|e| !e.all_day && e.end > now)
            .map(|e| e.id.clone())
            .next();
        for e in events {
            self.day_list
                .append(&event_row(e, now, next_id.as_ref() == Some(&e.id)));
        }
    }
}

fn event_row(e: &Meeting, now: i64, is_next: bool) -> gtk::Box {
    let when = if e.all_day {
        "All day".to_owned()
    } else {
        format!("{} – {}", clock(e.start), clock(e.end))
    };
    let text = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .hexpand(true)
        .build();
    text.append(
        &Label::builder()
            .label(&e.title)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["cal-event-title"])
            .build(),
    );
    let mut meta = when;
    if is_next {
        meta.push_str(" · ");
        meta.push_str(&relative(e.start, e.end, now));
    }
    text.append(
        &Label::builder()
            .label(&meta)
            .xalign(0.0)
            .css_classes(["time"])
            .build(),
    );
    let row = gtk::Box::builder()
        .spacing(8)
        .css_classes(if e.all_day {
            ["cal-event", "all-day"]
        } else {
            ["cal-event", "timed"]
        })
        .build();
    row.append(&text);
    if let Some(url) = e.join_url.clone() {
        let join = control_button(&["round", "join"], Some("atoll-videocam-symbolic"));
        join.set_tooltip_text(Some("Join"));
        join.connect_clicked(move |_| {
            if let Err(err) =
                gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>)
            {
                tracing::warn!("cannot open meeting link: {err}");
            }
        });
        row.append(&join);
    }
    row
}

/// Local days an event touches (all-day ends are exclusive midnights).
fn days_covered(e: &Meeting) -> Vec<NaiveDate> {
    let day = |ts: i64| Local.timestamp_opt(ts, 0).single().map(|t| t.date_naive());
    let (Some(first), Some(last)) = (day(e.start), day((e.end - 1).max(e.start))) else {
        return Vec::new();
    };
    first
        .iter_days()
        .take_while(|d| *d <= last)
        .take(62)
        .collect()
}

pub fn clock(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// "in 12 min", "in 2 h 5 min", "now · 20 min left", "tomorrow".
pub fn relative(start: i64, end: i64, now: i64) -> String {
    if start <= now {
        let left = (end - now).max(0) / 60;
        return format!("now · {left} min left");
    }
    let mins = (start - now + 59) / 60;
    match mins {
        m if m < 60 => format!("in {m} min"),
        m if m < 12 * 60 => format!("in {} h {} min", m / 60, m % 60),
        _ => {
            let day = |ts: i64| Local.timestamp_opt(ts, 0).single().map(|t| t.date_naive());
            if day(start) == day(now) {
                "later today".into()
            } else {
                "tomorrow".into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::relative;

    #[test]
    fn relative_meeting_times() {
        let now = 1_000_000;
        assert_eq!(relative(now + 5 * 60, now + 65 * 60, now), "in 5 min");
        assert_eq!(relative(now + 30, now + 3600, now), "in 1 min", "rounds up");
        assert_eq!(
            relative(now + 125 * 60, now + 185 * 60, now),
            "in 2 h 5 min"
        );
        assert_eq!(relative(now - 60, now + 20 * 60, now), "now · 20 min left");
    }
}
