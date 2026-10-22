//! To-do card: today's progress and the task list, fully editable in the
//! notch. Clicking a task ticks it off; hovering shows edit and delete.
//! Adding or editing text needs the keyboard, so while an entry is open the
//! card reports `on_typing(true)` and the island takes keyboard focus; it is
//! handed back when typing ends (Enter, Esc, or focus moving elsewhere).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib, Align, Label, Orientation};

use super::{control_button, live_clock};
use crate::todo::{Command, Task, View};

/// How typing ended: whether focus should go back to the previous window.
/// Not when the user clicked elsewhere; focus is already where they want.
#[derive(Clone, Copy)]
pub enum Typing {
    Started,
    Ended { restore_focus: bool },
}

pub struct TodoCard {
    pub root: gtk::Box,
    count: Label,
    status: Label,
    actions: gtk::Stack,
    new_entry: gtk::Entry,
    list: gtk::Box,
    empty: Label,
    commands: async_channel::Sender<Command>,
    on_typing: Rc<dyn Fn(Typing)>,
    /// Task being edited inline, if any.
    editing: RefCell<Option<u64>>,
    tasks: RefCell<Vec<Task>>,
}

impl TodoCard {
    pub fn new(
        commands: async_channel::Sender<Command>,
        on_typing: impl Fn(Typing) + 'static,
    ) -> Rc<Self> {
        // Top row sits under the GNOME bar (no clicks there): labels only.
        let title = gtk::Box::builder().spacing(6).build();
        title.append(
            &gtk::Image::builder()
                .icon_name("atoll-check-symbolic")
                .css_classes(["todo-icon"])
                .build(),
        );
        title.append(
            &Label::builder()
                .label("TO-DO")
                .css_classes(["caps", "lavender"])
                .build(),
        );
        let time = Label::builder().css_classes(["clock"]).build();
        live_clock(&time, "%H:%M:%S");
        let top = gtk::CenterBox::builder()
            .css_classes(["card-top"])
            .start_widget(&title)
            .center_widget(&time)
            .build();

        let count = Label::builder()
            .css_classes(["water-amount"])
            .xalign(0.0)
            .build();
        let status = Label::builder()
            .css_classes(["subtitle"])
            .xalign(0.0)
            .build();

        // The Add button turns into a text field in place.
        let add = control_button(&["pill", "primary-pill"], None);
        add.set_child(Some(&icon_label("atoll-add-symbolic", "Add task")));
        add.set_halign(Align::Start);
        let new_entry = gtk::Entry::builder()
            .placeholder_text("New task, Enter to add")
            .css_classes(["todo-entry"])
            .build();
        let actions = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .css_classes(["water-actions"])
            .build();
        actions.add_named(&add, Some("button"));
        actions.add_named(&new_entry, Some("entry"));

        let left = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .width_request(180)
            .spacing(4)
            .valign(Align::Center)
            .build();
        left.append(&count);
        left.append(&status);
        left.append(&actions);

        let list = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(6)
            .build();
        let empty = Label::builder()
            .label("Nothing to do. Add a task to get started.")
            .css_classes(["subtitle"])
            .wrap(true)
            .valign(Align::Center)
            .vexpand(true)
            .build();
        list.append(&empty);
        // Scrolls on its own; scrolling here doesn't flip cards.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .min_content_height(124)
            .max_content_height(124)
            .child(&list)
            .build();

        let body = gtk::Box::builder()
            .spacing(24)
            .css_classes(["todo-body"])
            .build();
        body.append(&left);
        body.append(&scroller);

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["water-card"])
            .width_request(crate::island::EXPANDED_TODO.0 as i32)
            .height_request(crate::island::EXPANDED_TODO.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&top);
        root.append(&body);

        let card = Rc::new(Self {
            root,
            count,
            status,
            actions,
            new_entry,
            list,
            empty,
            commands,
            on_typing: Rc::new(on_typing),
            editing: RefCell::default(),
            tasks: RefCell::default(),
        });

        let weak = Rc::downgrade(&card);
        add.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.start_adding();
            }
        });
        let weak = Rc::downgrade(&card);
        card.new_entry.connect_activate(move |e| {
            let Some(c) = weak.upgrade() else { return };
            let text = e.text().trim().to_owned();
            if text.is_empty() {
                c.stop_adding(true);
                return;
            }
            // Stay open for the next task; Esc or empty Enter finishes.
            let _ = c.commands.try_send(Command::Add(text));
            e.set_text("");
        });
        let weak = Rc::downgrade(&card);
        on_escape(&card.new_entry, move || {
            if let Some(c) = weak.upgrade() {
                c.stop_adding(true);
            }
        });
        let weak = Rc::downgrade(&card);
        on_focus_lost(&card.new_entry, move || {
            if let Some(c) = weak.upgrade() {
                if c.actions.visible_child_name().as_deref() == Some("entry") {
                    c.stop_adding(false);
                }
            }
        });
        card
    }

    pub fn update(self: &Rc<Self>, v: &View) {
        let total = v.tasks.len();
        self.count.set_label(&format!("{}/{}", v.done_today, total));
        self.status.set_label(match (total, v.done_today) {
            (0, _) => "No tasks yet",
            (t, d) if d == t => "All done today",
            _ => "Done today",
        });
        self.tasks.replace(v.tasks.clone());
        // Don't rebuild under an open edit field; it refreshes when done.
        if self.editing.borrow().is_none() {
            self.rebuild();
        }
    }

    fn rebuild(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let tasks = self.tasks.borrow().clone();
        if tasks.is_empty() {
            self.list.append(&self.empty);
            return;
        }
        for task in &tasks {
            self.list.append(&self.task_row(task));
        }
    }

    fn task_row(self: &Rc<Self>, task: &Task) -> gtk::Box {
        let done = task.done_on.is_some();
        let check = gtk::Image::from_icon_name(if done {
            "atoll-check-circle-symbolic"
        } else {
            "atoll-circle-symbolic"
        });
        check.add_css_class("todo-check");
        let text = Label::builder()
            .label(&task.text)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let content = gtk::Box::builder().spacing(10).build();
        content.append(&check);
        content.append(&text);
        let toggle = gtk::Button::builder()
            .child(&content)
            .css_classes(["todo-toggle"])
            .hexpand(true)
            .focus_on_click(false)
            .can_focus(false)
            .build();
        toggle.set_cursor_from_name(Some("pointer"));
        let (tx, id) = (self.commands.clone(), task.id);
        toggle.connect_clicked(move |_| {
            let _ = tx.try_send(Command::Toggle(id));
        });

        let edit = control_button(&["todo-tool"], Some("atoll-edit-symbolic"));
        edit.set_tooltip_text(Some("Edit"));
        let delete = control_button(&["todo-tool"], Some("atoll-close-symbolic"));
        delete.set_tooltip_text(Some("Delete"));
        let tx = self.commands.clone();
        delete.connect_clicked(move |_| {
            let _ = tx.try_send(Command::Remove(id));
        });

        let row = gtk::Box::builder()
            .css_classes(if done {
                ["todo-row", "done"]
            } else {
                ["todo-row", "open"]
            })
            .spacing(2)
            .build();
        row.append(&toggle);
        row.append(&edit);
        row.append(&delete);

        let (weak, r, text) = (Rc::downgrade(self), row.downgrade(), task.text.clone());
        edit.connect_clicked(move |_| {
            if let (Some(c), Some(r)) = (weak.upgrade(), r.upgrade()) {
                c.start_editing(&r, id, &text);
            }
        });
        row
    }

    fn start_adding(self: &Rc<Self>) {
        self.actions.set_visible_child_name("entry");
        (self.on_typing)(Typing::Started);
        self.new_entry.grab_focus();
    }

    fn stop_adding(&self, restore_focus: bool) {
        self.new_entry.set_text("");
        self.actions.set_visible_child_name("button");
        (self.on_typing)(Typing::Ended { restore_focus });
    }

    /// Swaps the row's contents for a text field holding the task.
    fn start_editing(self: &Rc<Self>, row: &gtk::Box, id: u64, text: &str) {
        if self.editing.replace(Some(id)).is_some() {
            return; // one edit at a time
        }
        while let Some(child) = row.first_child() {
            row.remove(&child);
        }
        let entry = gtk::Entry::builder()
            .text(text)
            .hexpand(true)
            .css_classes(["todo-entry"])
            .build();
        row.append(&entry);
        (self.on_typing)(Typing::Started);
        entry.grab_focus();

        let weak = Rc::downgrade(self);
        entry.connect_activate(move |e| {
            if let Some(c) = weak.upgrade() {
                let _ = c.commands.try_send(Command::Edit(id, e.text().to_string()));
                c.stop_editing(true);
            }
        });
        let weak = Rc::downgrade(self);
        on_escape(&entry, move || {
            if let Some(c) = weak.upgrade() {
                c.stop_editing(true);
            }
        });
        let weak = Rc::downgrade(self);
        on_focus_lost(&entry, move || {
            if let Some(c) = weak.upgrade() {
                if c.editing.borrow().is_some() {
                    c.stop_editing(false);
                }
            }
        });
    }

    fn stop_editing(self: &Rc<Self>, restore_focus: bool) {
        if self.editing.take().is_none() {
            return;
        }
        // Rebuild from idle: we're inside a callback of the entry being
        // removed.
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(c) = weak.upgrade() {
                c.rebuild();
            }
        });
        (self.on_typing)(Typing::Ended { restore_focus });
    }
}

fn on_escape(entry: &gtk::Entry, f: impl Fn() + 'static) {
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            f();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(keys);
}

fn on_focus_lost(entry: &gtk::Entry, f: impl Fn() + 'static) {
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| f());
    entry.add_controller(focus);
}

fn icon_label(icon: &str, text: &str) -> gtk::Box {
    let b = gtk::Box::builder().spacing(6).build();
    b.append(&gtk::Image::from_icon_name(icon));
    b.append(&Label::new(Some(text)));
    b
}
