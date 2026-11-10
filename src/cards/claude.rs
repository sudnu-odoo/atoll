//! Claude Code prompts in the notch: tool permissions (Allow, Always allow,
//! Deny, or a typed reply) and questions (a list of options with their
//! descriptions, plus a typed "Other" answer). Renders a
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
use super::todo::Typing;
use crate::services::claude::{Choice, Question, Reply, Request, ANSWER_TIMEOUT};

// Wrapping labels below use `max_width_chars(1)` + `hexpand`: they ask for
// almost no width and wrap to whatever the fixed-width card gives them.
// Capping by a character count instead overflowed the notch whenever the
// font's real glyphs were wider than Pango's estimate (long paths in the
// monospace font).
/// Long commands show this tall, then scroll inside their box.
const COMMAND_MAX_H: i32 = 84;
/// Option lists taller than this scroll, so the card fits the notch.
const OPTIONS_MAX_H: i32 = 230;

pub struct ClaudeCard {
    pub root: gtk::Box,
    project: Label,
    countdown: Label,
    ring: gtk::DrawingArea,
    fraction: Rc<Cell<f64>>,
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
    /// Typing a reply takes keyboard focus (and gives it back after).
    on_typing: Rc<dyn Fn(Typing)>,
    typing: Cell<bool>,
}

impl ClaudeCard {
    pub fn new(
        answers: async_channel::Sender<Reply>,
        on_typing: impl Fn(Typing) + 'static,
    ) -> Rc<Self> {
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
        // Project names can be long ("17.0-my-client-feature-branch");
        // they shorten rather than widen the card.
        // The project's own name, as written (long uppercase paths were
        // hard to scan).
        let project = Label::builder()
            .css_classes(["claude-project", "muted"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .hexpand(true)
            .build();
        super::full_text_on_hover(&project);
        title.append(&project);
        title.set_hexpand(true);
        let countdown = Label::builder().css_classes(["time", "muted"]).build();
        // Time left, at a glance: a ring that empties as the prompt waits.
        let fraction = Rc::new(Cell::new(1.0));
        let ring = gtk::DrawingArea::builder()
            .content_width(14)
            .content_height(14)
            .valign(Align::Center)
            .build();
        let f = fraction.clone();
        ring.set_draw_func(move |_, cr, w, h| {
            super::widgets::draw_ring(cr, f64::from(w), f64::from(h), f.get());
        });
        // A plain box, not a CenterBox: a CenterBox asks for twice its widest
        // side, which made the card wider than the notch.
        // Its own header: a prompt has no clock or tabs to make room for.
        let top = gtk::Box::builder()
            .css_classes(["claude-top"])
            .spacing(12)
            .build();
        top.append(&title);
        top.append(&ring);
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
            ring,
            fraction,
            body,
            answers,
            pending: RefCell::default(),
            collected: RefCell::default(),
            left: Cell::new(0),
            ticker: RefCell::default(),
            on_layout: RefCell::default(),
            on_typing: Rc::new(on_typing),
            typing: Cell::new(false),
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
                always,
            } => {
                self.project.set_label(&format!("· {project}"));
                self.permission(tool, summary, always.as_deref());
            }
            Request::Question { questions, project } => {
                self.project.set_label(&format!("· {project}"));
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
        self.end_typing();
    }

    fn end_typing(&self) {
        if self.typing.replace(false) {
            (self.on_typing)(Typing::Ended {
                restore_focus: true,
            });
        }
    }

    /// Replaces the action row with a text field. Enter sends `submit`'s
    /// reply, Esc goes back to the buttons.
    fn ask_text(
        self: &Rc<Self>,
        row: &gtk::Box,
        placeholder: &str,
        submit: impl Fn(&Rc<Self>, String) + 'static,
    ) {
        let buttons: Vec<gtk::Widget> = {
            let mut v = Vec::new();
            let mut c = row.first_child();
            while let Some(w) = c {
                c = w.next_sibling();
                v.push(w);
            }
            v
        };
        for b in &buttons {
            b.set_visible(false);
        }
        let entry = gtk::Entry::builder()
            .placeholder_text(placeholder)
            .hexpand(true)
            .css_classes(["claude-entry"])
            .build();
        let send = control_button(&["pill", "primary-pill"], None);
        send.set_label("Send");
        row.set_halign(Align::Fill);
        row.append(&entry);
        row.append(&send);

        let submit = Rc::new(submit);
        let go = {
            let (weak, entry, submit) = (Rc::downgrade(self), entry.clone(), submit.clone());
            Rc::new(move || {
                let text = entry.text().trim().to_owned();
                if text.is_empty() {
                    return;
                }
                if let Some(card) = weak.upgrade() {
                    card.end_typing();
                    submit(&card, text);
                }
            })
        };
        let g = go.clone();
        entry.connect_activate(move |_| g());
        send.connect_clicked(move |_| go());
        let keys = gtk::EventControllerKey::new();
        let (weak, row_w, entry_w, send_w) = (
            Rc::downgrade(self),
            row.clone(),
            entry.clone(),
            send.clone(),
        );
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            row_w.remove(&entry_w);
            row_w.remove(&send_w);
            row_w.set_halign(Align::End);
            for b in &buttons {
                b.set_visible(true);
            }
            if let Some(card) = weak.upgrade() {
                card.end_typing();
            }
            glib::Propagation::Stop
        });
        entry.add_controller(keys);

        self.typing.set(true);
        (self.on_typing)(Typing::Started);
        entry.grab_focus();
    }

    fn clear(&self) {
        self.stop();
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
    }

    fn permission(self: &Rc<Self>, tool: &str, summary: &str, always: Option<&str>) {
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

        let actions = gtk::Box::builder().spacing(10).halign(Align::End).build();
        // A typed reply denies with a reason Claude reads ("use pnpm
        // instead"), like typing in Claude Code's own prompt.
        let reply = control_button(&["pill"], None);
        let content = gtk::Box::builder().spacing(6).build();
        content.append(&gtk::Image::from_icon_name("atoll-edit-symbolic"));
        content.append(&Label::new(Some("Reply…")));
        reply.set_child(Some(&content));
        reply.set_tooltip_text(Some("Deny and tell Claude what to do instead"));
        let (weak, row) = (Rc::downgrade(self), actions.clone());
        reply.connect_clicked(move |_| {
            if let Some(card) = weak.upgrade() {
                card.ask_text(&row, "Tell Claude what to do instead…", |card, text| {
                    let _ = card.answers.try_send(Reply::DenyWith(text));
                });
            }
        });
        actions.append(&reply);
        actions.append(&self.action_button("Deny", "atoll-close-symbolic", &["pill"], Reply::Deny));
        if let Some(rule) = always {
            let b = self.action_button(
                "Always allow",
                "atoll-check-circle-symbolic",
                &["pill"],
                Reply::AllowAlways,
            );
            b.set_tooltip_text(Some(&format!("Allow, and don't ask again for {rule}")));
            actions.append(&b);
        }
        actions.append(&self.action_button(
            "Allow",
            "atoll-check-symbolic",
            &["pill", "primary-pill"],
            Reply::Allow,
        ));

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

        // The question's topic joins the header line instead of a chip.
        if !q.header.is_empty() {
            let base = self
                .project
                .label()
                .split(" · ")
                .take(1)
                .collect::<Vec<_>>()
                .join(" · ");
            self.project.set_label(&format!("{base} · {}", q.header));
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
        list.append(&self.other_row(&q));
        // Many options would push the last ones (and Other…) past the
        // notch's maximum height: the list scrolls in place instead.
        let options = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(OPTIONS_MAX_H)
            .child(&list)
            .build();
        self.body.append(&options);

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

    /// "Other…": a typed answer instead of the listed options, like Claude
    /// Code's own picker offers.
    fn other_row(self: &Rc<Self>, q: &Question) -> gtk::Widget {
        let text = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(2)
            .build();
        text.append(
            &Label::builder()
                .label("Other…")
                .css_classes(["option-label"])
                .xalign(0.0)
                .build(),
        );
        text.append(
            &Label::builder()
                .label("Type your own answer")
                .css_classes(["option-description"])
                .xalign(0.0)
                .build(),
        );
        let b = gtk::Button::builder()
            .child(&text)
            .css_classes(["option-row"])
            .focus_on_click(false)
            .can_focus(false)
            .build();
        b.set_cursor_from_name(Some("pointer"));
        let (weak, question) = (Rc::downgrade(self), q.question.clone());
        b.connect_clicked(move |b| {
            let Some(card) = weak.upgrade() else { return };
            // The field takes the row's place.
            let row = gtk::Box::builder().spacing(10).build();
            if let Some(list) = b.parent().and_downcast::<gtk::Box>() {
                list.insert_child_after(&row, Some(b));
                b.set_visible(false);
            }
            let question = question.clone();
            card.ask_text(&row, "Your answer…", move |card, text| {
                card.collected.borrow_mut().insert(question.clone(), text);
                card.next_question();
            });
            if let Some(f) = card.on_layout.borrow().as_ref() {
                f();
            };
        });
        b.upcast()
    }

    fn start_countdown(self: &Rc<Self>) {
        self.left.set(ANSWER_TIMEOUT.as_secs());
        self.countdown.set_label(&format!("{}s", self.left.get()));
        self.fraction.set(1.0);
        self.ring.queue_draw();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(card) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let left = card.left.get().saturating_sub(1);
            card.left.set(left);
            card.countdown.set_label(&format!("{left}s"));
            card.fraction
                .set(left as f64 / ANSWER_TIMEOUT.as_secs().max(1) as f64);
            card.ring.queue_draw();
            if left == 0 {
                card.ticker.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.ticker.replace(Some(id));
    }
}
