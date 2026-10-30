//! To-do card, in Nusa's to-do layout (the user's choice): progress and two
//! actions on the left; on the right either the task list or, while adding
//! or editing, a form (name, start – end time, Cancel / Save).
//!
//! Clicking a task ticks it off; in edit mode (✎ Edit Task) clicking a task
//! opens it in the form instead. While the form is open the card reports
//! `on_typing` so the island takes keyboard focus, and hands it back after.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib, Align, DrawingArea, Label, Orientation};

use super::control_button;
use super::widgets::draw_ring;
use crate::todo::{Command, Draft, Task, View};

/// Height of the right panel (list or form).
const PANEL_H: i32 = 132;

/// How typing ended: whether focus should go back to the previous window.
/// Not when the user clicked elsewhere; focus is already where they want.
#[derive(Clone, Copy)]
pub enum Typing {
    Started,
    Ended { restore_focus: bool },
}

/// The form's fields.
struct Form {
    root: gtk::Box,
    name: gtk::Entry,
    start: (gtk::Entry, gtk::Entry),
    end: (gtk::Entry, gtk::Entry),
    cancel: gtk::Button,
    delete: gtk::Button,
    save: gtk::Button,
}

pub struct TodoCard {
    pub root: gtk::Box,
    count: Label,
    ring: DrawingArea,
    done_fraction: Rc<Cell<f64>>,
    edit_toggle: gtk::Button,
    panel: gtk::Stack,
    list: gtk::Box,
    empty: Label,
    form: Form,
    commands: async_channel::Sender<Command>,
    on_typing: Rc<dyn Fn(Typing)>,
    /// Edit mode: clicking a task opens it instead of ticking it off.
    edit_mode: Cell<bool>,
    /// The form is open: `Some(None)` adding, `Some(Some(id))` editing.
    form_for: Cell<Option<Option<u64>>>,
    tasks: RefCell<Vec<Task>>,
}

impl TodoCard {
    pub fn new(
        commands: async_channel::Sender<Command>,
        on_typing: impl Fn(Typing) + 'static,
    ) -> Rc<Self> {
        let done_fraction = Rc::new(Cell::new(0.0));
        let ring = DrawingArea::builder()
            .content_width(14)
            .content_height(14)
            .valign(Align::Center)
            .build();
        let f = done_fraction.clone();
        ring.set_draw_func(move |_, cr, w, h| draw_ring(cr, f64::from(w), f64::from(h), f.get()));
        // The island's shared top row (tabs, clock, settings) names the card
        // and shows the time; this row keeps only the card's own actions.
        // The ring (share of tasks done) is the card's own information.
        let top = gtk::CenterBox::builder()
            .css_classes(["card-top"])
            .start_widget(&ring)
            .build();

        // Left: big progress, label, then the two actions.
        // Count and caption centered over the two action buttons.
        let count = Label::builder()
            .css_classes(["todo-count"])
            .halign(Align::Center)
            .build();
        let caption = Label::builder()
            .label("Task Done Today")
            .css_classes(["subtitle", "todo-caption"])
            .halign(Align::Center)
            .build();
        let add = control_button(&["todo-action", "add"], None);
        add.set_child(Some(&icon_label("atoll-add-symbolic", "Add Task")));
        let edit_toggle = control_button(&["todo-action"], None);
        edit_toggle.set_child(Some(&icon_label("atoll-edit-symbolic", "Edit Task")));
        let actions = gtk::Box::builder()
            .spacing(14)
            .css_classes(["todo-actions"])
            .build();
        actions.append(&add);
        actions.append(&edit_toggle);
        let left = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .valign(Align::Center)
            .hexpand(false)
            .build();
        left.append(&count);
        left.append(&caption);
        left.append(&actions);

        // Right: the list, or the form.
        let list = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(6)
            .css_classes(["todo-list"])
            .build();
        let empty = Label::builder()
            .label("Nothing to do yet")
            .css_classes(["subtitle"])
            .valign(Align::Center)
            .vexpand(true)
            .build();
        // A visible scrollbar, like the reference; scrolling here doesn't
        // flip cards.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .overlay_scrolling(false)
            .min_content_height(PANEL_H)
            .max_content_height(PANEL_H)
            .css_classes(["todo-scroller"])
            .child(&list)
            .build();
        let form = build_form();
        let panel = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .hexpand(true)
            .valign(Align::Start)
            .build();
        panel.add_named(&scroller, Some("list"));
        panel.add_named(&form.root, Some("form"));

        let body = gtk::Box::builder()
            .spacing(26)
            .css_classes(["todo-body"])
            .build();
        body.append(&left);
        body.append(&panel);

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
            ring,
            done_fraction,
            edit_toggle,
            panel,
            list,
            empty,
            form,
            commands,
            on_typing: Rc::new(on_typing),
            edit_mode: Cell::new(false),
            form_for: Cell::new(None),
            tasks: RefCell::default(),
        });
        card.connect(&add);
        card.rebuild();
        card
    }

    fn connect(self: &Rc<Self>, add: &gtk::Button) {
        let weak = Rc::downgrade(self);
        add.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.open_form(None);
            }
        });
        let weak = Rc::downgrade(self);
        self.edit_toggle.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.set_edit_mode(!c.edit_mode.get());
            }
        });

        let f = &self.form;
        // Save only once there's a name.
        let save = f.save.clone();
        f.name
            .connect_changed(move |e| save.set_sensitive(!e.text().trim().is_empty()));
        let weak = Rc::downgrade(self);
        f.save.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.save();
            }
        });
        let weak = Rc::downgrade(self);
        f.cancel.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.close_form(true);
            }
        });
        let weak = Rc::downgrade(self);
        f.delete.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                if let Some(Some(id)) = c.form_for.get() {
                    let _ = c.commands.try_send(Command::Remove(id));
                }
                c.close_form(true);
            }
        });
        for e in [&f.name, &f.start.0, &f.start.1, &f.end.0, &f.end.1] {
            let weak = Rc::downgrade(self);
            e.connect_activate(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.save();
                }
            });
            let weak = Rc::downgrade(self);
            on_escape(e, move || {
                if let Some(c) = weak.upgrade() {
                    c.close_form(true);
                }
            });
        }
        // Clicking another window ends typing; focus stays where they clicked.
        let weak = Rc::downgrade(self);
        on_focus_lost(&f.root, move || {
            if let Some(c) = weak.upgrade() {
                if c.form_for.get().is_some() {
                    c.close_form(false);
                }
            }
        });
    }

    pub fn update(self: &Rc<Self>, v: &View) {
        let total = v.tasks.len();
        self.count.set_label(&format!("{}/{}", v.done_today, total));
        self.done_fraction.set(if total == 0 {
            0.0
        } else {
            v.done_today as f64 / total as f64
        });
        self.ring.queue_draw();
        self.tasks.replace(v.tasks.clone());
        self.rebuild();
    }

    /// Developer aid: the form as it looks, without taking focus.
    pub fn show_form_for_debugging(&self, editing: bool) {
        self.form.delete.set_visible(editing);
        if editing {
            self.form.name.set_text("meeting with client");
            fill(&self.form.start, Some("09:20"));
            fill(&self.form.end, Some("10:30"));
        }
        self.panel.set_visible_child_name("form");
    }

    fn set_edit_mode(self: &Rc<Self>, on: bool) {
        self.edit_mode.set(on);
        self.edit_toggle.set_child(Some(&icon_label(
            if on {
                "atoll-check-symbolic"
            } else {
                "atoll-edit-symbolic"
            },
            if on { "Done" } else { "Edit Task" },
        )));
        if on {
            self.edit_toggle.add_css_class("on");
        } else {
            self.edit_toggle.remove_css_class("on");
        }
        self.rebuild();
    }

    fn open_form(self: &Rc<Self>, task: Option<&Task>) {
        if self.form_for.get().is_some() {
            return;
        }
        let f = &self.form;
        f.name.set_text(task.map_or("", |t| t.text.as_str()));
        fill(&f.start, task.and_then(|t| t.time.as_deref()));
        fill(&f.end, task.and_then(|t| t.end_time.as_deref()));
        f.save.set_sensitive(task.is_some());
        f.delete.set_visible(task.is_some());
        self.form_for.set(Some(task.map(|t| t.id)));
        self.panel.set_visible_child_name("form");
        (self.on_typing)(Typing::Started);
        f.name.grab_focus();
    }

    fn close_form(self: &Rc<Self>, restore_focus: bool) {
        if self.form_for.take().is_none() {
            return;
        }
        self.panel.set_visible_child_name("list");
        (self.on_typing)(Typing::Ended { restore_focus });
    }

    fn save(self: &Rc<Self>) {
        let f = &self.form;
        let text = f.name.text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let draft = Draft {
            text,
            start: time_text(&f.start),
            end: time_text(&f.end),
        };
        match self.form_for.get() {
            Some(Some(id)) => {
                let _ = self.commands.try_send(Command::Edit(id, draft));
            }
            Some(None) => {
                let _ = self.commands.try_send(Command::Add(draft));
            }
            None => return,
        }
        self.close_form(true);
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

    fn task_row(self: &Rc<Self>, task: &Task) -> gtk::Button {
        let done = task.done_on.is_some();
        let check = gtk::Image::from_icon_name(if done {
            "atoll-check-circle-symbolic"
        } else {
            "atoll-circle-symbolic"
        });
        check.add_css_class("todo-check");
        let lines = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Center)
            .build();
        lines.append(
            &Label::builder()
                .label(&task.text)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["todo-text"])
                .build(),
        );
        if let Some(start) = &task.time {
            let range = match &task.end_time {
                Some(end) => format!("{start}-{end}"),
                None => start.clone(),
            };
            lines.append(
                &Label::builder()
                    .label(range)
                    .xalign(0.0)
                    .css_classes(["todo-time"])
                    .build(),
            );
        }
        let content = gtk::Box::builder().spacing(10).build();
        content.append(&check);
        content.append(&lines);
        if self.edit_mode.get() {
            let pencil = gtk::Image::from_icon_name("atoll-edit-symbolic");
            pencil.add_css_class("todo-pencil");
            content.append(&pencil);
        }
        let row = gtk::Button::builder()
            .child(&content)
            .css_classes(if done {
                ["todo-row", "done"]
            } else {
                ["todo-row", "open"]
            })
            .focus_on_click(false)
            .can_focus(false)
            .build();
        row.set_cursor_from_name(Some("pointer"));
        let (weak, task) = (Rc::downgrade(self), task.clone());
        row.connect_clicked(move |_| {
            let Some(c) = weak.upgrade() else { return };
            if c.edit_mode.get() {
                c.open_form(Some(&task));
            } else {
                let _ = c.commands.try_send(Command::Toggle(task.id));
            }
        });
        row
    }
}

/// Name field, start – end time, Cancel / Save (and Delete when editing).
fn build_form() -> Form {
    let name = gtk::Entry::builder()
        .placeholder_text("Task name")
        .secondary_icon_name("atoll-close-symbolic")
        .secondary_icon_tooltip_text("Clear")
        .css_classes(["todo-field"])
        .build();
    name.connect_icon_press(|e, pos| {
        if pos == gtk::EntryIconPosition::Secondary {
            e.set_text("");
        }
    });

    let start = (digits("HH"), digits("MM"));
    let end = (digits("HH"), digits("MM"));
    // Typing two digits moves on to the next box.
    let chain = [&start.0, &start.1, &end.0, &end.1];
    for pair in chain.windows(2) {
        let next = pair[1].clone();
        pair[0].connect_changed(move |e| {
            if e.text().len() == 2 && e.has_focus() {
                next.grab_focus();
            }
        });
    }
    let clock = gtk::Image::from_icon_name("atoll-clock-symbolic");
    clock.add_css_class("todo-clock");
    let sep = |t: &str| Label::builder().label(t).css_classes(["todo-sep"]).build();
    let time = gtk::Box::builder()
        .css_classes(["todo-field", "todo-time-field"])
        .tooltip_text("Optional: start and end time")
        .build();
    time.append(&clock);
    time.append(&start.0);
    time.append(&sep(":"));
    time.append(&start.1);
    time.append(&sep("–"));
    time.append(&end.0);
    time.append(&sep(":"));
    time.append(&end.1);

    let text_button = |label: &str, extra: &[&str]| {
        let mut classes = vec!["todo-text-button"];
        classes.extend_from_slice(extra);
        let b = gtk::Button::builder()
            .label(label)
            .css_classes(classes)
            .focus_on_click(false)
            .build();
        b.set_cursor_from_name(Some("pointer"));
        b
    };
    let cancel = text_button("Cancel", &[]);
    let delete = text_button("Delete", &["danger"]);
    delete.set_visible(false);
    let save = control_button(&["pill", "todo-save"], None);
    save.set_label("Save");
    save.set_hexpand(true);
    save.set_halign(Align::Fill);
    // Equal widths, so Cancel / Delete / Save read as one row.
    let buttons = gtk::Box::builder()
        .spacing(8)
        .homogeneous(true)
        .css_classes(["todo-form-buttons"])
        .build();
    buttons.append(&delete);
    buttons.append(&cancel);
    buttons.append(&save);

    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    root.append(&name);
    root.append(&time);
    root.append(&buttons);
    Form {
        root,
        name,
        start,
        end,
        cancel,
        delete,
        save,
    }
}

/// A two-digit box; non-digits are dropped as typed.
fn digits(placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .max_length(2)
        .width_chars(2)
        .max_width_chars(2)
        .input_purpose(gtk::InputPurpose::Digits)
        .xalign(0.5)
        .css_classes(["todo-digits"])
        .build();
    e.connect_changed(|e| {
        let text = e.text();
        let only: String = text.chars().filter(char::is_ascii_digit).collect();
        if only != text.as_str() {
            e.set_text(&only);
        }
    });
    e
}

fn fill((hh, mm): &(gtk::Entry, gtk::Entry), time: Option<&str>) {
    let (h, m) = time.and_then(|t| t.split_once(':')).unwrap_or(("", ""));
    hh.set_text(h);
    mm.set_text(m);
}

/// "9" + "" -> "9:00"; empty hour -> no time.
fn time_text((hh, mm): &(gtk::Entry, gtk::Entry)) -> String {
    match (hh.text().trim(), mm.text().trim()) {
        ("", _) => String::new(),
        (h, "") => format!("{h}:00"),
        (h, m) => format!("{h}:{m}"),
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

/// Focus left `widget` and everything inside it.
fn on_focus_lost(widget: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| f());
    widget.add_controller(focus);
}

fn icon_label(icon: &str, text: &str) -> gtk::Box {
    let b = gtk::Box::builder().spacing(6).build();
    b.append(&gtk::Image::from_icon_name(icon));
    b.append(&Label::new(Some(text)));
    b
}
