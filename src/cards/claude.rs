//! Claude Code prompts in the notch: tool permissions (Allow / Deny) and
//! questions (a list of options with their descriptions). Renders a
//! `claude::Request` and sends the user's `Reply`; the hub routes it back to
//! the waiting hook.
//!
//! The card is a fixed width and as tall as its content; the island asks
//! `natural_height()` and grows the notch to fit (up to a limit).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{glib, Align, Label, Orientation};

use super::control_button;
use crate::services::claude::{Choice, Question, Reply, Request, ANSWER_TIMEOUT};

// Wrapping labels below use `max_width_chars(1)` + `hexpand`: they ask for
// almost no width and wrap to whatever the fixed-width card gives them.
// Capping by a character count instead overflowed the notch whenever the
// font's real glyphs were wider than Pango's estimate (long paths in the
// monospace font).
/// Long commands show this tall, then scroll inside their box.
const COMMAND_MAX_H: i32 = 84;

pub struct ClaudeCard {
    pub root: gtk::Box,
    project: Label,
    countdown: Label,
    body: gtk::Box,
    answers: async_channel::Sender<Reply>,
    /// Questions still to ask, and answers collected so far.
    pending: RefCell<Vec<Question>>,
    collected: RefCell<HashMap<String, String>>,
    left: Cell<u64>,
    ticker: RefCell<Option<glib::SourceId>>,
    /// Called when the content changes size (next question), so the island
    /// can resize the notch.
    on_layout: RefCell<Option<Box<dyn Fn()>>>,
}

impl ClaudeCard {
    pub fn new(answers: async_channel::Sender<Reply>) -> Rc<Self> {
        // Top row sits under the GNOME bar: labels only.
        let title = gtk::Box::builder().spacing(6).build();
        title.append(
            &gtk::Image::builder()
                .icon_name("atoll-claude-symbolic")
                .css_classes(["claude-icon"])
                .build(),
        );
        title.append(
            &Label::builder()
                .label("CLAUDE")
                .css_classes(["caps", "accent"])
                .build(),
        );
        // Project names can be long ("19.0-sejatibrands-6634496-sudnu");
        // they shorten rather than widen the card.
        let project = Label::builder()
            .css_classes(["caps", "muted"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .hexpand(true)
            .build();
        super::full_text_on_hover(&project);
        title.append(&project);
        title.set_hexpand(true);
        let countdown = Label::builder().css_classes(["time"]).build();
        // A plain box, not a CenterBox: a CenterBox asks for twice its widest
        // side, which made the card wider than the notch.
        let top = gtk::Box::builder()
            .css_classes(["card-top"])
            .spacing(12)
            .build();
        top.append(&title);
        top.append(&countdown);

        let body = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(10)
            .build();
        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["claude-card"])
            .width_request(crate::island::EXPANDED_CLAUDE.0 as i32)
            // Centered at its own width: filling would stretch it to the
            // page container, which is as wide as the widest card.
            .halign(Align::Center)
            .valign(Align::Start)
            .build();
        root.append(&top);
        root.append(&body);

        Rc::new(Self {
            root,
            project,
            countdown,
            body,
            answers,
            pending: RefCell::default(),
            collected: RefCell::default(),
            left: Cell::new(0),
            ticker: RefCell::default(),
            on_layout: RefCell::default(),
        })
    }

    pub fn set_on_layout(&self, f: impl Fn() + 'static) {
        self.on_layout.replace(Some(Box::new(f)));
    }

    /// Height the card needs at its fixed width.
    pub fn natural_height(&self) -> i32 {
        let width = crate::island::EXPANDED_CLAUDE.0 as i32;
        self.root.measure(Orientation::Vertical, width).1
    }

    pub fn show(self: &Rc<Self>, request: &Request) {
        self.clear();
        match request {
            Request::Permission {
                tool,
                summary,
                project,
            } => {
                self.project
                    .set_label(&format!("· {}", project.to_uppercase()));
                self.permission(tool, summary);
            }
            Request::Question { questions, project } => {
                self.project
                    .set_label(&format!("· {}", project.to_uppercase()));
                self.collected.replace(HashMap::new());
                let mut queue = questions.clone();
                queue.reverse(); // pop() takes them in order
                self.pending.replace(queue);
                self.next_question();
            }
            // Finished / waiting are peeks, not prompts.
            _ => return,
        }
        self.start_countdown();
    }

    /// Stops the countdown; the hub calls this when the prompt goes away.
    pub fn stop(&self) {
        if let Some(id) = self.ticker.take() {
            id.remove();
        }
    }

    fn clear(&self) {
        self.stop();
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
    }

    fn permission(self: &Rc<Self>, tool: &str, summary: &str) {
        let heading = gtk::Box::builder().spacing(10).build();
        heading.append(
            &Label::builder()
                .label("Wants to use")
                .css_classes(["claude-title"])
                .build(),
        );
        let chip = gtk::Box::builder()
            .spacing(5)
            .css_classes(["chip"])
            .valign(Align::Center)
            .build();
        chip.append(&gtk::Image::from_icon_name("atoll-terminal-symbolic"));
        chip.append(&Label::new(Some(tool)));
        heading.append(&chip);

        // The full command, wrapped; beyond a few lines it scrolls in place.
        let what = Label::builder()
            .label(summary)
            .css_classes(["claude-command-text"])
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(1)
            .hexpand(true)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .css_classes(["claude-command"])
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(COMMAND_MAX_H)
            .child(&what)
            .build();

        let deny = self.action_button("Deny", "atoll-close-symbolic", &["pill"], Reply::Deny);
        let allow = self.action_button(
            "Allow",
            "atoll-check-symbolic",
            &["pill", "primary-pill"],
            Reply::Allow,
        );
        let actions = gtk::Box::builder().spacing(10).halign(Align::End).build();
        actions.append(&deny);
        actions.append(&allow);

        self.body.append(&heading);
        self.body.append(&scroller);
        self.body.append(&actions);
    }

    fn action_button(&self, text: &str, icon: &str, classes: &[&str], reply: Reply) -> gtk::Button {
        let b = control_button(classes, None);
        let content = gtk::Box::builder().spacing(6).build();
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(&Label::new(Some(text)));
        b.set_child(Some(&content));
        let tx = self.answers.clone();
        b.connect_clicked(move |_| {
            let _ = tx.try_send(reply.clone());
        });
        b
    }

    /// Shows the next queued question, or sends all answers when done.
    fn next_question(self: &Rc<Self>) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        let Some(q) = self.pending.borrow_mut().pop() else {
            let answers = self.collected.take();
            let _ = self.answers.try_send(Reply::Answers(answers));
            return;
        };

        if !q.header.is_empty() {
            self.body.append(
                &Label::builder()
                    .label(&q.header)
                    .css_classes(["chip"])
                    .halign(Align::Start)
                    .build(),
            );
        }
        self.body.append(
            &Label::builder()
                .label(&q.question)
                .css_classes(["claude-title"])
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .max_width_chars(1)
                .hexpand(true)
                .build(),
        );

        // A list, like Claude Code's own picker: each option shows its
        // description, so there's no hovering to find out what it means.
        let list = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["option-list"])
            .build();
        let picked: Rc<RefCell<Vec<String>>> = Rc::default();
        for choice in &q.options {
            list.append(&self.option_row(&q, choice, &picked));
        }
        self.body.append(&list);

        if q.multi_select {
            let done = control_button(&["pill", "primary-pill"], None);
            done.set_label("Done");
            done.set_halign(Align::End);
            let (weak, question) = (Rc::downgrade(self), q.question.clone());
            done.connect_clicked(move |_| {
                if let Some(card) = weak.upgrade() {
                    let joined = picked.borrow().join(", ");
                    card.collected.borrow_mut().insert(question.clone(), joined);
                    card.next_question();
                }
            });
            self.body.append(&done);
        }

        if let Some(f) = self.on_layout.borrow().as_ref() {
            f();
        }
    }

    fn option_row(
        self: &Rc<Self>,
        q: &Question,
        choice: &Choice,
        picked: &Rc<RefCell<Vec<String>>>,
    ) -> gtk::Widget {
        let text = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(2)
            .build();
        text.append(
            &Label::builder()
                .label(&choice.label)
                .css_classes(["option-label"])
                .xalign(0.0)
                .wrap(true)
                .max_width_chars(1)
                .hexpand(true)
                .build(),
        );
        if !choice.description.is_empty() {
            text.append(
                &Label::builder()
                    .label(&choice.description)
                    .css_classes(["option-description"])
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(1)
                    .hexpand(true)
                    .build(),
            );
        }

        let label = choice.label.clone();
        if q.multi_select {
            let t = gtk::ToggleButton::builder()
                .child(&text)
                .css_classes(["option-row"])
                .focus_on_click(false)
                .can_focus(false)
                .build();
            t.set_cursor_from_name(Some("pointer"));
            let picked = picked.clone();
            t.connect_toggled(move |t| {
                let mut p = picked.borrow_mut();
                p.retain(|l| *l != label);
                if t.is_active() {
                    p.push(label.clone());
                }
            });
            return t.upcast();
        }
        let b = gtk::Button::builder()
            .child(&text)
            .css_classes(["option-row"])
            .focus_on_click(false)
            .can_focus(false)
            .build();
        b.set_cursor_from_name(Some("pointer"));
        let (weak, question) = (Rc::downgrade(self), q.question.clone());
        b.connect_clicked(move |_| {
            if let Some(card) = weak.upgrade() {
                card.collected
                    .borrow_mut()
                    .insert(question.clone(), label.clone());
                card.next_question();
            }
        });
        b.upcast()
    }

    fn start_countdown(self: &Rc<Self>) {
        self.left.set(ANSWER_TIMEOUT.as_secs());
        self.countdown.set_label(&format!("{}s", self.left.get()));
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(card) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let left = card.left.get().saturating_sub(1);
            card.left.set(left);
            card.countdown.set_label(&format!("{left}s"));
            if left == 0 {
                card.ticker.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.ticker.replace(Some(id));
    }
}
