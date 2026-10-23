//! Water tracking: today's log (persisted), the view the card shows, and
//! reminders. Reminders are measured from the last drink, not a fixed
//! clock, and only fire during the configured active hours.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::cards::peek::{Kind, Peek};
use crate::config;
use crate::store;

const FILE: &str = "water.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Log {
    /// "YYYY-MM-DD"; a log from another day is discarded on load.
    day: String,
    drinks: Vec<Drink>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Drink {
    /// Minutes since local midnight.
    minute: u32,
    ml: u32,
}

/// What the water card renders.
#[derive(Clone, Debug)]
pub struct View {
    pub total_ml: u32,
    pub goal_ml: u32,
    pub glass_ml: u32,
    /// ml drunk in each hour from `first_hour` to the end of active hours.
    pub per_hour: Vec<u32>,
    pub first_hour: u32,
    pub current_hour: u32,
    pub reminders: bool,
    /// Minutes until the next reminder, if one is scheduled today.
    pub next_reminder_min: Option<u32>,
    pub can_undo: bool,
}

pub enum Command {
    Drink,
    Undo,
    ToggleReminders,
}

pub struct Water {
    config: RefCell<config::Water>,
    log: RefCell<Log>,
    on_view: Box<dyn Fn(View)>,
    on_peek: Box<dyn Fn(Peek)>,
    reminder: RefCell<Option<glib::SourceId>>,
}

fn now() -> Option<(String, u32)> {
    let t = glib::DateTime::now_local().ok()?;
    let day = t.format("%Y-%m-%d").ok()?.to_string();
    let minute = u32::try_from(t.hour() * 60 + t.minute()).ok()?;
    Some((day, minute))
}

impl Water {
    pub fn new(
        config: config::Water,
        on_view: impl Fn(View) + 'static,
        on_peek: impl Fn(Peek) + 'static,
    ) -> Rc<Self> {
        let log = store::data_file(FILE)
            .and_then(|p| {
                store::read::<Log>(&p).unwrap_or_else(|e| {
                    tracing::warn!("water log unreadable, starting fresh: {e}");
                    None
                })
            })
            .unwrap_or_default();
        let water = Rc::new(Self {
            config: RefCell::new(config),
            log: RefCell::new(log),
            on_view: Box::new(on_view),
            on_peek: Box::new(on_peek),
            reminder: RefCell::new(None),
        });
        water.roll_day();
        water.reschedule();
        water
    }

    pub fn handle(self: &Rc<Self>, cmd: Command) {
        self.roll_day();
        let Some((_, minute)) = now() else { return };
        match cmd {
            Command::Drink => {
                let ml = self.config.borrow().glass_ml;
                self.log.borrow_mut().drinks.push(Drink { minute, ml });
                self.save();
            }
            Command::Undo => {
                if self.log.borrow_mut().drinks.pop().is_some() {
                    self.save();
                }
            }
            Command::ToggleReminders => {
                let mut c = self.config.borrow_mut();
                c.reminders = !c.reminders;
            }
        }
        self.reschedule();
        self.publish();
    }

    pub fn set_config(self: &Rc<Self>, config: config::Water) {
        self.config.replace(config);
        self.reschedule();
        self.publish();
    }

    pub fn config(&self) -> config::Water {
        self.config.borrow().clone()
    }

    /// Recomputes the view (e.g. once a minute, so "next in N min" stays
    /// right and the day rolls over at midnight).
    pub fn publish(&self) {
        self.roll_day();
        if let Some(view) = self.view() {
            (self.on_view)(view);
        }
    }

    pub fn view(&self) -> Option<View> {
        let (_, minute) = now()?;
        let c = self.config.borrow();
        let log = self.log.borrow();
        let first_hour = c.active_from_hour.min(minute / 60);
        let last_hour = c.active_to_hour.max(minute / 60 + 1).min(24);
        let mut per_hour = vec![0; (last_hour - first_hour) as usize];
        for d in &log.drinks {
            let h = d.minute / 60;
            if let Some(slot) = h
                .checked_sub(first_hour)
                .and_then(|i| per_hour.get_mut(i as usize))
            {
                *slot += d.ml;
            }
        }
        Some(View {
            total_ml: log.drinks.iter().map(|d| d.ml).sum(),
            goal_ml: c.goal_ml,
            glass_ml: c.glass_ml,
            per_hour,
            first_hour,
            current_hour: minute / 60,
            reminders: c.reminders,
            next_reminder_min: next_due(&c, &log, minute).map(|due| due - minute),
            can_undo: !log.drinks.is_empty(),
        })
    }

    fn reschedule(self: &Rc<Self>) {
        if let Some(id) = self.reminder.take() {
            id.remove();
        }
        let Some((_, minute)) = now() else { return };
        let due = {
            let c = self.config.borrow();
            let log = self.log.borrow();
            next_due(&c, &log, minute)
        };
        // Nothing left today: check again just after midnight.
        let wait_min = due.map_or(24 * 60 - minute + 1, |d| d.saturating_sub(minute).max(1));
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local_once(wait_min * 60, move || {
            if let Some(w) = weak.upgrade() {
                w.reminder.take();
                w.remind();
            }
        });
        self.reminder.replace(Some(id));
    }

    fn remind(self: &Rc<Self>) {
        self.roll_day();
        let fire = now().is_some_and(|(_, minute)| {
            let c = self.config.borrow();
            let log = self.log.borrow();
            next_due(&c, &log, minute).is_some_and(|due| due <= minute)
        });
        if fire {
            if let Some(v) = self.view() {
                (self.on_peek)(reminder_peek(&v));
            }
            self.snooze();
            return;
        }
        self.reschedule();
    }

    /// After a reminder, wait a full interval before the next one, even
    /// though the last drink is still old.
    fn snooze(self: &Rc<Self>) {
        let every = self.config.borrow().remind_every_min;
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local_once(every * 60, move || {
            if let Some(w) = weak.upgrade() {
                w.reminder.take();
                w.remind();
            }
        });
        self.reminder.replace(Some(id));
    }

    fn roll_day(&self) {
        let Some((day, _)) = now() else { return };
        let mut log = self.log.borrow_mut();
        if log.day != day {
            *log = Log {
                day,
                drinks: Vec::new(),
            };
        }
    }

    fn save(&self) {
        let Some(path) = store::data_file(FILE) else {
            return;
        };
        if let Err(e) = store::write(&path, &*self.log.borrow()) {
            tracing::warn!("cannot save water log: {e}");
        }
    }
}

fn reminder_peek(v: &View) -> Peek {
    let litres = |ml: u32| format!("{:.1}", f64::from(ml) / 1000.0);
    Peek {
        kind: Kind::Water,
        title: "Time for some water".into(),
        subtitle: format!("{} / {} L today", litres(v.total_ml), litres(v.goal_ml)),
        percent: Some(100.0 * f64::from(v.total_ml) / f64::from(v.goal_ml.max(1))),
    }
}

/// Minute of today when the next reminder is due, if any: an interval after
/// the last drink (or the start of active hours), never before now, and
/// only inside active hours.
fn next_due(c: &config::Water, log: &Log, minute: u32) -> Option<u32> {
    if !c.reminders {
        return None;
    }
    let (from, to) = (c.active_from_hour * 60, c.active_to_hour * 60);
    let since = log.drinks.last().map_or(from, |d| d.minute.max(from));
    let due = (since + c.remind_every_min).max(minute.max(from));
    (due < to).then_some(due)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> config::Water {
        config::Water {
            goal_ml: 2000,
            glass_ml: 250,
            reminders: true,
            remind_every_min: 60,
            active_from_hour: 9,
            active_to_hour: 21,
        }
    }

    fn log(drinks: &[u32]) -> Log {
        Log {
            day: "2026-10-23".into(),
            drinks: drinks
                .iter()
                .map(|&minute| Drink { minute, ml: 250 })
                .collect(),
        }
    }

    #[test]
    fn first_reminder_is_an_interval_after_active_hours_start() {
        // 07:00, nothing drunk: due 10:00 (09:00 + 60).
        assert_eq!(next_due(&cfg(), &log(&[]), 7 * 60), Some(10 * 60));
    }

    #[test]
    fn counts_from_the_last_drink() {
        // Drank at 11:15: due 12:15.
        assert_eq!(
            next_due(&cfg(), &log(&[600, 675]), 11 * 60 + 20),
            Some(12 * 60 + 15)
        );
    }

    #[test]
    fn overdue_means_now() {
        // Last drink 10:00, it's 13:00: due right now.
        assert_eq!(next_due(&cfg(), &log(&[600]), 13 * 60), Some(13 * 60));
    }

    #[test]
    fn nothing_outside_active_hours_or_when_off() {
        assert_eq!(next_due(&cfg(), &log(&[20 * 60 + 30]), 20 * 60 + 40), None);
        let off = config::Water {
            reminders: false,
            ..cfg()
        };
        assert_eq!(next_due(&off, &log(&[]), 10 * 60), None);
    }
}
