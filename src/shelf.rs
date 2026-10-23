//! File shelf: files dropped on the notch, kept as links (paths) rather than
//! copies, so nothing is duplicated on disk. Saved across restarts; files that
//! no longer exist drop off when the shelf is shown.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store;

const FILE: &str = "shelf.json";
/// Enough for a working set; older items fall off the end.
const MAX_ITEMS: usize = 24;

#[derive(Default, Serialize, Deserialize)]
struct Store {
    /// Most recently added first.
    paths: Vec<PathBuf>,
}

pub enum Command {
    Add(Vec<PathBuf>),
    Remove(PathBuf),
    Clear,
}

pub struct Shelf {
    store: RefCell<Store>,
    on_view: Box<dyn Fn(Vec<PathBuf>)>,
}

impl Shelf {
    pub fn new(on_view: impl Fn(Vec<PathBuf>) + 'static) -> Self {
        let store = store::data_file(FILE)
            .and_then(|p| {
                store::read::<Store>(&p).unwrap_or_else(|e| {
                    tracing::warn!("shelf unreadable, starting empty: {e}");
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
                Command::Add(paths) => {
                    for p in paths.into_iter().rev() {
                        s.paths.retain(|q| *q != p);
                        s.paths.insert(0, p);
                    }
                    s.paths.truncate(MAX_ITEMS);
                }
                Command::Remove(p) => s.paths.retain(|q| *q != p),
                Command::Clear => s.paths.clear(),
            }
        }
        self.save();
        self.publish();
    }

    pub fn publish(&self) {
        (self.on_view)(self.items());
    }

    /// Current items, minus files that were moved or deleted meanwhile.
    pub fn items(&self) -> Vec<PathBuf> {
        let mut s = self.store.borrow_mut();
        let before = s.paths.len();
        s.paths.retain(|p| Path::new(p).exists());
        let changed = s.paths.len() != before;
        let items = s.paths.clone();
        drop(s);
        if changed {
            self.save();
        }
        items
    }

    fn save(&self) {
        let Some(path) = store::data_file(FILE) else {
            return;
        };
        if let Err(e) = store::write(&path, &*self.store.borrow()) {
            tracing::warn!("cannot save shelf: {e}");
        }
    }
}
