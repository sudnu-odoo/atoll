//! To-do list: tasks persisted locally, and the view the card shows. Open
//! tasks stay until done or deleted; done tasks stay visible for the rest of
//! the day they were finished, then drop off.

use std::cell::RefCell;

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::store;

const FILE: &str = "todo.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub text: String,
    /// "YYYY-MM-DD" of completion; `None` while open.
    pub done_on: Option<String>,
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

pub enum Command {
    Add(String),
    Toggle(u64),
    Edit(u64, String),
    Remove(u64),
}

pub struct Todo {
    store: RefCell<Store>,
    on_view: Box<dyn Fn(View)>,
}

fn today() -> String {
    glib::DateTime::now_local()
        .ok()
        .and_then(|t| t.format("%Y-%m-%d").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

impl Todo {
    pub fn new(on_view: impl Fn(View) + 'static) -> Self {
        let store = store::data_file(FILE)
            .and_then(|p| {
                store::read::<Store>(&p).unwrap_or_else(|e| {
                    // Keep the broken file for the user; start empty in memory.
                    tracing::warn!("to-do list unreadable, not overwriting it: {e}");
                    None
                })
            })
            .unwrap_or_default();
        Self {
            store: RefCell::new(store),
            on_view: Box::new(on_view),
        }
    }

    pub fn handle(&self, cmd: Command) {
        {
            let mut s = self.store.borrow_mut();
            match cmd {
                Command::Add(text) => {
                    let text = text.trim().to_owned();
                    if text.is_empty() {
                        return;
                    }
                    s.next_id += 1;
                    let id = s.next_id;
                    s.tasks.push(Task {
                        id,
                        text,
                        done_on: None,
                    });
                }
                Command::Toggle(id) => {
                    if let Some(t) = s.tasks.iter_mut().find(|t| t.id == id) {
                        t.done_on = match t.done_on {
                            Some(_) => None,
                            None => Some(today()),
                        };
                    }
                }
                Command::Edit(id, text) => {
                    let text = text.trim().to_owned();
                    if let Some(t) = s.tasks.iter_mut().find(|t| t.id == id) {
                        if !text.is_empty() {
                            t.text = text;
                        }
                    }
                }
                Command::Remove(id) => s.tasks.retain(|t| t.id != id),
            }
            // Done tasks from earlier days have served their purpose.
            let day = today();
            s.tasks
                .retain(|t| t.done_on.as_ref().is_none_or(|d| *d == day));
        }
        self.save();
        self.publish();
    }

    pub fn publish(&self) {
        (self.on_view)(self.view());
    }

    pub fn view(&self) -> View {
        let day = today();
        let s = self.store.borrow();
        let mut tasks: Vec<Task> = s
            .tasks
            .iter()
            .filter(|t| t.done_on.as_ref().is_none_or(|d| *d == day))
            .cloned()
            .collect();
        // Open first (in the order added), then done.
        tasks.sort_by_key(|t| t.done_on.is_some());
        let done_today = tasks.iter().filter(|t| t.done_on.is_some()).count();
        View { tasks, done_today }
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
