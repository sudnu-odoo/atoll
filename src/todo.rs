//! To-do list: tasks persisted locally, and the view the card shows. Open
//! tasks stay until done or deleted; done tasks stay visible for the rest of
//! the day they were finished, then drop off. A task may carry a time of
//! day; open timed tasks get a reminder peek at that time, once per day.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::cards::peek::{Kind, Peek};
use crate::store;

const FILE: &str = "todo.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub text: String,
    /// "YYYY-MM-DD" of completion; `None` while open.
    pub done_on: Option<String>,
    /// Optional "HH:MM", local time, every day until done; the reminder
    /// fires at this time.
    #[serde(default)]
    pub time: Option<String>,
    /// Optional end of the time range ("20:00-21:00").
    #[serde(default)]
    pub end_time: Option<String>,
    /// "YYYY-MM-DD" the reminder last fired, so it fires once a day.
    #[serde(default)]
    pub reminded_on: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    next_id: u64,
    tasks: Vec<Task>,
}

/// What the card renders: today's relevant tasks, open ones first.
#[derive(Clone, Debug)]
pub struct View {
    pub tasks: Vec<Task>,
    pub done_today: usize,
}

/// Text and optional start/end times as typed (parsed leniently).
pub struct Draft {
    pub text: String,
    pub start: String,
    pub end: String,
}

pub enum Command {
    Add(Draft),
    Toggle(u64),
    Edit(u64, Draft),
    Remove(u64),
}

pub struct Todo {
    store: RefCell<Store>,
    on_view: Box<dyn Fn(View)>,
    on_peek: Box<dyn Fn(Peek)>,
    reminder: RefCell<Option<glib::SourceId>>,
}

fn today() -> String {
    glib::DateTime::now_local()
        .ok()
        .and_then(|t| t.format("%Y-%m-%d").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

impl Todo {
    pub fn new(on_view: impl Fn(View) + 'static, on_peek: impl Fn(Peek) + 'static) -> Rc<Self> {
        let store = store::data_file(FILE)
            .and_then(|p| {
                store::read::<Store>(&p).unwrap_or_else(|e| {
                    // Keep the broken file for the user; start empty in memory.
                    tracing::warn!("to-do list unreadable, not overwriting it: {e}");
                    None
                })
            })
            .unwrap_or_default();
        let todo = Rc::new(Self {
            store: RefCell::new(store),
            on_view: Box::new(on_view),
            on_peek: Box::new(on_peek),
            reminder: RefCell::default(),
        });
        todo.schedule();
        todo
    }

    pub fn handle(self: &Rc<Self>, cmd: Command) {
        if !apply(&mut self.store.borrow_mut(), cmd, &today()) {
            return;
        }
        self.save();
        self.publish();
        self.schedule();
    }

    pub fn publish(&self) {
        (self.on_view)(self.view());
    }

    pub fn view(&self) -> View {
        view(&self.store.borrow(), &today())
    }

    /// Arms one timer for the next timed task due today.
    fn schedule(self: &Rc<Self>) {
        if let Some(id) = self.reminder.take() {
            id.remove();
        }
        let Some((day, now_min)) = now() else { return };
        let next = self
            .store
            .borrow()
            .tasks
            .iter()
            .filter(|t| t.done_on.is_none() && t.reminded_on.as_deref() != Some(day.as_str()))
            .filter_map(|t| minutes(t.time.as_deref()?))
            .filter(|m| *m >= now_min)
            .min();
        // Nothing left today: look again just after midnight.
        let wait_min = next.map_or(24 * 60 - now_min + 1, |m| m - now_min);
        let wait = (wait_min * 60).saturating_sub(seconds_into_minute()).max(1);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local_once(wait, move || {
            if let Some(t) = weak.upgrade() {
                t.reminder.take();
                t.remind_due();
                t.schedule();
            }
        });
        self.reminder.replace(Some(id));
    }

    fn remind_due(&self) {
        let Some((day, now_min)) = now() else { return };
        let mut due = Vec::new();
        for t in self.store.borrow_mut().tasks.iter_mut() {
            let is_due = t.done_on.is_none()
                && t.reminded_on.as_deref() != Some(day.as_str())
                && t.time
                    .as_deref()
                    .and_then(minutes)
                    .is_some_and(|m| m <= now_min);
            if is_due {
                t.reminded_on = Some(day.clone());
                due.push((t.text.clone(), t.time.clone().unwrap_or_default()));
            }
        }
        if due.is_empty() {
            return;
        }
        self.save();
        for (text, time) in due {
            (self.on_peek)(Peek {
                kind: Kind::Todo,
                title: text,
                subtitle: format!("To-do · due {time}"),
                percent: None,
            });
        }
    }

    fn save(&self) {
        let Some(path) = store::data_file(FILE) else {
            return;
        };
        if let Err(e) = store::write(&path, &*self.store.borrow()) {
            tracing::warn!("cannot save to-do list: {e}");
        }
    }
}

fn now() -> Option<(String, u32)> {
    let t = glib::DateTime::now_local().ok()?;
    let day = t.format("%Y-%m-%d").ok()?.to_string();
    Some((day, u32::try_from(t.hour() * 60 + t.minute()).ok()?))
}

fn seconds_into_minute() -> u32 {
    glib::DateTime::now_local().map_or(0, |t| t.second().unsigned_abs())
}

/// "HH:MM" -> minutes since midnight.
fn minutes(hhmm: &str) -> Option<u32> {
    let (h, m) = hhmm.split_once(':')?;
    Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
}

/// Lenient time input: "15:30", "9:05", "1530", "930", "3pm", "3:30 pm".
/// Anything else (including empty) means no time.
pub fn parse_time(input: &str) -> Option<String> {
    let s = input.trim().to_ascii_lowercase().replace(' ', "");
    if s.is_empty() {
        return None;
    }
    let (s, pm, am) = if let Some(x) = s.strip_suffix("pm") {
        (x.to_owned(), true, false)
    } else if let Some(x) = s.strip_suffix("am") {
        (x.to_owned(), false, true)
    } else {
        (s, false, false)
    };
    let (h, m): (u32, u32) = match s.split_once(['.', ':']) {
        Some((h, m)) => (h.parse().ok()?, m.parse().ok()?),
        None if s.len() >= 3 => {
            let (h, m) = s.split_at(s.len() - 2);
            (h.parse().ok()?, m.parse().ok()?)
        }
        None => (s.parse().ok()?, 0),
    };
    let h = match (pm, am, h) {
        (true, _, 1..=11) => h + 12,
        (_, true, 12) => 0,
        _ => h,
    };
    (h < 24 && m < 60).then(|| format!("{h:02}:{m:02}"))
}

/// Applies one change to the list. False when nothing changed (an empty
/// task), so nothing needs saving.
fn apply(s: &mut Store, cmd: Command, day: &str) -> bool {
    match cmd {
        Command::Add(d) => {
            let text = d.text.trim().to_owned();
            if text.is_empty() {
                return false;
            }
            s.next_id += 1;
            let id = s.next_id;
            let time = parse_time(&d.start);
            s.tasks.push(Task {
                id,
                text,
                done_on: None,
                // An end time alone means nothing.
                end_time: time.as_ref().and_then(|_| parse_time(&d.end)),
                time,
                reminded_on: None,
            });
        }
        Command::Toggle(id) => {
            if let Some(t) = s.tasks.iter_mut().find(|t| t.id == id) {
                t.done_on = match t.done_on {
                    Some(_) => None,
                    None => Some(day.to_owned()),
                };
            }
        }
        Command::Edit(id, d) => {
            let text = d.text.trim().to_owned();
            if let Some(t) = s.tasks.iter_mut().find(|t| t.id == id) {
                if !text.is_empty() {
                    t.text = text;
                }
                let time = parse_time(&d.start);
                t.end_time = time.as_ref().and_then(|_| parse_time(&d.end));
                if time != t.time {
                    t.time = time;
                    t.reminded_on = None; // new time: remind again
                }
            }
        }
        Command::Remove(id) => s.tasks.retain(|t| t.id != id),
    }
    // Done tasks from earlier days have served their purpose.
    s.tasks
        .retain(|t| t.done_on.as_ref().is_none_or(|d| d == day));
    true
}

/// Today's tasks: open first (timed ones by time, then untimed), then done.
fn view(s: &Store, day: &str) -> View {
    let mut tasks: Vec<Task> = s
        .tasks
        .iter()
        .filter(|t| t.done_on.as_ref().is_none_or(|d| d == day))
        .cloned()
        .collect();
    tasks.sort_by(|a, b| {
        (a.done_on.is_some(), a.time.is_none(), &a.time).cmp(&(
            b.done_on.is_some(),
            b.time.is_none(),
            &b.time,
        ))
    });
    let done_today = tasks.iter().filter(|t| t.done_on.is_some()).count();
    View { tasks, done_today }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: &str = "2026-10-23";

    fn draft(text: &str, start: &str, end: &str) -> Draft {
        Draft {
            text: text.into(),
            start: start.into(),
            end: end.into(),
        }
    }

    #[test]
    fn lenient_times() {
        for (input, want) in [
            ("15:30", Some("15:30")),
            ("9:05", Some("09:05")),
            ("1530", Some("15:30")),
            ("930", Some("09:30")),
            ("3pm", Some("15:00")),
            ("3:30 PM", Some("15:30")),
            ("12am", Some("00:00")),
            ("12pm", Some("12:00")),
            ("9", Some("09:00")),
            ("", None),
            ("25:00", None),
            ("9:75", None),
            ("soon", None),
        ] {
            assert_eq!(parse_time(input).as_deref(), want, "{input}");
        }
    }

    #[test]
    fn add_trims_and_rejects_empty() {
        let mut s = Store::default();
        assert!(!apply(&mut s, Command::Add(draft("   ", "", "")), DAY));
        assert!(apply(
            &mut s,
            Command::Add(draft("  Call Dina ", "15:30", "16:00")),
            DAY
        ));
        let t = &s.tasks[0];
        assert_eq!(t.text, "Call Dina");
        assert_eq!(t.time.as_deref(), Some("15:30"));
        assert_eq!(t.end_time.as_deref(), Some("16:00"));
    }

    #[test]
    fn end_time_needs_a_start() {
        let mut s = Store::default();
        apply(&mut s, Command::Add(draft("x", "", "16:00")), DAY);
        assert_eq!(s.tasks[0].time, None);
        assert_eq!(s.tasks[0].end_time, None);
    }

    #[test]
    fn toggle_edit_remove() {
        let mut s = Store::default();
        apply(&mut s, Command::Add(draft("a", "9:00", "")), DAY);
        let id = s.tasks[0].id;
        apply(&mut s, Command::Toggle(id), DAY);
        assert_eq!(s.tasks[0].done_on.as_deref(), Some(DAY));
        apply(&mut s, Command::Toggle(id), DAY);
        assert_eq!(s.tasks[0].done_on, None);

        s.tasks[0].reminded_on = Some(DAY.into());
        apply(&mut s, Command::Edit(id, draft("b", "10:00", "")), DAY);
        assert_eq!(s.tasks[0].text, "b");
        assert_eq!(s.tasks[0].time.as_deref(), Some("10:00"));
        assert_eq!(
            s.tasks[0].reminded_on, None,
            "a new time re-arms the reminder"
        );

        apply(&mut s, Command::Edit(id, draft("  ", "10:00", "")), DAY);
        assert_eq!(s.tasks[0].text, "b", "an empty edit keeps the text");

        apply(&mut s, Command::Remove(id), DAY);
        assert!(s.tasks.is_empty());
    }

    #[test]
    fn yesterdays_done_tasks_drop_off() {
        let mut s = Store::default();
        apply(&mut s, Command::Add(draft("old", "", "")), DAY);
        apply(&mut s, Command::Add(draft("open", "", "")), DAY);
        s.tasks[0].done_on = Some("2026-10-22".into());
        apply(&mut s, Command::Add(draft("new", "", "")), DAY);
        let names: Vec<&str> = s.tasks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(names, ["open", "new"]);
    }

    #[test]
    fn view_orders_timed_open_then_untimed_then_done() {
        let mut s = Store::default();
        for (text, time) in [
            ("untimed", ""),
            ("late", "18:00"),
            ("early", "08:00"),
            ("done", "07:00"),
        ] {
            apply(&mut s, Command::Add(draft(text, time, "")), DAY);
        }
        let done = s
            .tasks
            .iter()
            .find(|t| t.text == "done")
            .map(|t| t.id)
            .unwrap();
        apply(&mut s, Command::Toggle(done), DAY);
        let v = view(&s, DAY);
        let names: Vec<&str> = v.tasks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(names, ["early", "late", "untimed", "done"]);
        assert_eq!(v.done_today, 1);
    }

    #[test]
    fn minutes_of_day() {
        assert_eq!(minutes("00:00"), Some(0));
        assert_eq!(minutes("15:30"), Some(930));
        assert_eq!(minutes("nope"), None);
    }
}
