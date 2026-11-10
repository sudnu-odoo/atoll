//! Wiring: starts services once, keeps one island per monitor, and fans
//! every service update out to all islands.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, Application, ApplicationWindow};

use crate::cards::peek::{Kind, Peek};
use crate::config::Config;
use crate::island::{self, Island};
use crate::services::{bluez, calendar, claude, controls, lyrics, mpris, notifications, upower};
use crate::{autostart, banners, settings, shelf, theme, todo, water, window};

/// Notifications kept for the card.
const RECENT_NOTIFICATIONS: usize = 30;

/// A dock connect fires several monitor changes in a row; act once it settles.
const MONITORS_SETTLE: Duration = Duration::from_millis(400);

struct Hub {
    app: Application,
    islands: RefCell<HashMap<String, (Rc<Island>, ApplicationWindow)>>,
    deps: RefCell<island::Deps>,
    config: RefCell<Config>,
    /// The newest version a peek has announced (announced once).
    update_told: RefCell<Option<String>>,
    water: RefCell<Option<Rc<water::Water>>>,
    todo: RefCell<Option<Rc<todo::Todo>>>,
    shelf: RefCell<Option<Rc<shelf::Shelf>>>,
    /// Newest first.
    notifications: RefCell<Vec<notifications::Notification>>,
    /// Upcoming meetings (sorted), and which ones were already announced.
    meetings: RefCell<Vec<calendar::Meeting>>,
    reminded: RefCell<std::collections::HashSet<String>>,
    reminder: RefCell<Option<glib::SourceId>>,
    settings: RefCell<Option<glib::WeakRef<gtk::Window>>>,
    /// Replayed to islands created later (a monitor plugged in mid-song).
    last_track: RefCell<Option<mpris::Update>>,
    last_art: RefCell<Option<mpris::Update>>,
    settle: RefCell<Option<glib::SourceId>>,
    /// Claude prompts waiting their turn, and the one on screen (with its
    /// timeout).
    claude_queue: RefCell<VecDeque<claude::Incoming>>,
    claude_current: RefCell<Option<(claude::Incoming, glib::SourceId)>>,
    claude_watch: RefCell<Option<glib::SourceId>>,
}

thread_local! {
    // The one hub, so `atoll --settings` (forwarded from a second launch)
    // can reach it.
    static HUB: RefCell<Option<Rc<Hub>>> = const { RefCell::new(None) };
}

/// Opens (or raises) the settings window in the running instance.
pub fn open_settings() {
    if let Some(hub) = HUB.with(|h| h.borrow().clone()) {
        hub.open_settings();
    }
}

/// True in the instance that built the islands (not in a second launch
/// that only forwarded its command line).
pub fn is_primary() -> bool {
    HUB.with(|h| h.borrow().is_some())
}

pub fn build(app: &Application) {
    // `activate` also runs when a second launch is forwarded here.
    if HUB.with(|h| h.borrow().is_some()) {
        return;
    }
    let config = Config::load();
    theme::apply(&config.theme);
    // Switched off in Settings, Atoll has no windows yet must keep running
    // (to be switched back on); the hold lasts for the whole process.
    std::mem::forget(app.hold());

    let (music_tx, music_rx) = async_channel::unbounded();
    let (lyrics_tx, lyrics_rx) = async_channel::unbounded();
    let (power_tx, power_rx) = async_channel::unbounded();
    let (bt_tx, bt_rx) = async_channel::unbounded();
    let (water_tx, water_rx) = async_channel::unbounded();
    let (settings_tx, settings_rx) = async_channel::unbounded();
    let (translate_tx, translate_rx) = async_channel::unbounded::<bool>();
    let (dnd_tx, dnd_rx) = async_channel::unbounded::<bool>();
    let (claude_tx, claude_rx) = async_channel::unbounded();
    let (answer_tx, answer_rx) = async_channel::unbounded();
    let (cal_tx, cal_rx) = async_channel::unbounded();
    let calendar_months = calendar::spawn(cal_tx);
    let (todo_tx, todo_rx) = async_channel::unbounded();
    let (shelf_tx, shelf_rx) = async_channel::unbounded();
    let (controls_tx, controls_rx) = async_channel::unbounded();
    let controls_commands = controls::spawn(controls_tx);
    let (notif_tx, notif_rx) = async_channel::unbounded();
    let (clear_tx, clear_rx) = async_channel::unbounded::<Option<u64>>();
    notifications::spawn(notif_tx);
    // GNOME's banners follow the setting; this also repairs them after a
    // crash, when the setting is now off but Atoll had hidden them.
    if config.hides_banners() {
        banners::hide();
    } else {
        banners::restore();
    }
    if !crate::preview::active() {
        autostart::apply(config.autostart);
        crate::shell::refresh_saved();
        crate::update::restart_when_replaced();
    }
    // The socket belongs to the user's running Atoll, never to a preview.
    if !crate::preview::active() {
        claude::spawn(claude_tx);
    }
    // Services always run (they idle on D-Bus signals at no cost); whether
    // their output is shown is decided per event, so toggling a feature in
    // settings takes effect immediately.
    upower::spawn(power_tx);
    bluez::spawn(bt_tx);

    let hub = Rc::new(Hub {
        update_told: RefCell::new(None),
        app: app.clone(),
        islands: RefCell::default(),
        deps: RefCell::new(island::Deps {
            // Previewing the clock card: as if nothing played.
            commands: if crate::preview::active()
                && std::env::var("ATOLL_SHOW").as_deref() == Ok("idle")
            {
                async_channel::unbounded().0
            } else {
                mpris::spawn(music_tx)
            },
            lyrics_requests: lyrics::spawn(lyrics_tx),
            water_commands: water_tx,
            open_settings: settings_tx,
            claude_answers: answer_tx,
            todo_commands: todo_tx,
            calendar_months,
            set_translate: translate_tx,
            set_dnd: dnd_tx,
            shelf_commands: shelf_tx,
            controls_commands,
            clear_notifications: clear_tx,
            features: config.features.clone(),
        }),
        water: RefCell::default(),
        todo: RefCell::default(),
        shelf: RefCell::default(),
        notifications: RefCell::default(),
        meetings: RefCell::default(),
        reminded: RefCell::default(),
        reminder: RefCell::default(),
        settings: RefCell::default(),
        config: RefCell::new(config.clone()),
        last_track: RefCell::default(),
        last_art: RefCell::default(),
        settle: RefCell::default(),
        claude_queue: RefCell::default(),
        claude_current: RefCell::default(),
        claude_watch: RefCell::default(),
    });
    HUB.with(|h| h.replace(Some(hub.clone())));
    // A minute after start (the network may still be coming up), then daily.
    let weak = Rc::downgrade(&hub);
    glib::timeout_add_local_once(Duration::from_secs(60), move || {
        if let Some(hub) = weak.upgrade() {
            hub.check_updates();
            let weak = Rc::downgrade(&hub);
            glib::timeout_add_local(Duration::from_secs(24 * 3600), move || {
                let Some(hub) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                hub.check_updates();
                glib::ControlFlow::Continue
            });
        }
    });
    // GNOME switched between light and dark: follow, if set to System.
    let weak = Rc::downgrade(&hub);
    theme::watch_system(move || {
        let Some(hub) = weak.upgrade() else { return };
        let theme = hub.config.borrow().theme.clone();
        if theme.appearance == crate::config::Appearance::System {
            theme::apply(&theme);
            hub.each(|i| redraw_all(i.root.upcast_ref()));
        }
    });

    let (h1, h2) = (Rc::downgrade(&hub), Rc::downgrade(&hub));
    let w = water::Water::new(
        config.water.clone(),
        move |view| {
            if let Some(hub) = h1.upgrade() {
                hub.each(|i| i.on_water(&view));
            }
        },
        move |peek| {
            if let Some(hub) = h2.upgrade() {
                if hub.config.borrow().features.water {
                    hub.each(|i| i.on_peek(peek.clone()));
                }
            }
        },
    );
    hub.water.replace(Some(w.clone()));
    let h = Rc::downgrade(&hub);
    forward(water_rx, move |cmd| {
        let toggles = matches!(cmd, water::Command::ToggleReminders);
        w.handle(cmd);
        // The bell is a setting: keep it across restarts.
        if let (true, Some(hub)) = (toggles, h.upgrade()) {
            let mut config = hub.config.borrow_mut();
            config.water = w.config();
            config.save();
        }
    });
    let (h1, h2) = (Rc::downgrade(&hub), Rc::downgrade(&hub));
    let t = todo::Todo::new(
        move |view| {
            if let Some(hub) = h1.upgrade() {
                hub.each(|i| i.on_todo(&view));
            }
        },
        move |peek| {
            if let Some(hub) = h2.upgrade() {
                if hub.config.borrow().features.todo {
                    hub.each(|i| i.on_peek(peek.clone()));
                }
            }
        },
    );
    hub.todo.replace(Some(t.clone()));
    forward(todo_rx, move |cmd| t.handle(cmd));

    let h = Rc::downgrade(&hub);
    forward(cal_rx, move |update| {
        let Some(hub) = h.upgrade() else { return };
        match update {
            calendar::Update::Upcoming(meetings) => {
                hub.meetings.replace(meetings);
                hub.schedule_reminder();
            }
            calendar::Update::Month {
                year,
                month,
                events,
            } => hub.each(|i| i.on_month(year, month, &events)),
        }
    });

    let h = Rc::downgrade(&hub);
    let sh = Rc::new(shelf::Shelf::new(move |items| {
        if let Some(hub) = h.upgrade() {
            hub.each(|i| i.on_shelf(&items));
        }
    }));
    hub.shelf.replace(Some(sh.clone()));
    let h = Rc::downgrade(&hub);
    forward(notif_rx, move |n: notifications::Notification| {
        let Some(hub) = h.upgrade() else { return };
        if !hub.config.borrow().features.notifications {
            return;
        }
        let peek = Peek {
            kind: Kind::Notification {
                icon: crate::cards::notifications::icon_for(&n),
            },
            title: if n.title.is_empty() {
                n.app.clone()
            } else {
                n.title.clone()
            },
            subtitle: [n.app.as_str(), n.body.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
            percent: None,
        };
        {
            let mut list = hub.notifications.borrow_mut();
            list.insert(0, n);
            list.truncate(RECENT_NOTIFICATIONS);
        }
        let items = hub.notifications.borrow().clone();
        // Do Not Disturb: kept in the list, but no peek.
        let quiet = hub.config.borrow().do_not_disturb;
        hub.each(|i| {
            i.on_notifications(&items);
            if !quiet {
                i.on_peek(peek.clone());
            }
        });
    });
    let h = Rc::downgrade(&hub);
    forward(clear_rx, move |which: Option<u64>| {
        if let Some(hub) = h.upgrade() {
            match which {
                Some(id) => hub.notifications.borrow_mut().retain(|n| n.id != id),
                None => hub.notifications.borrow_mut().clear(),
            }
            let items = hub.notifications.borrow().clone();
            hub.each(|i| i.on_notifications(&items));
        }
    });
    let h = Rc::downgrade(&hub);
    forward(controls_rx, move |state| {
        if let Some(hub) = h.upgrade() {
            hub.each(|i| i.on_controls(&state));
        }
    });
    forward(shelf_rx, move |cmd| sh.handle(cmd));

    // Once a minute keeps "next in N min" current and rolls the day over
    // (water log, and yesterday's done tasks leaving the list).
    let h = Rc::downgrade(&hub);
    crate::cards::every_minute(move |_| {
        let Some(hub) = h.upgrade() else { return };
        if let Some(w) = hub.water.borrow().clone() {
            w.publish();
        }
        let todo = hub.todo.borrow().clone();
        if let Some(t) = todo {
            t.publish();
        }
        hub.each(|i| i.tick_calendar());
    });

    let Some(display) = gdk::Display::default() else {
        tracing::error!("no display");
        return;
    };
    let monitors = display.monitors();
    hub.reconcile(&monitors);
    let weak = Rc::downgrade(&hub);
    monitors.connect_items_changed(move |list, _, _, _| {
        if let Some(hub) = weak.upgrade() {
            hub.schedule_reconcile(list);
        }
    });

    let h = hub.clone();
    forward(music_rx, move |update: mpris::Update| {
        match &update {
            mpris::Update::Track(_) => h.last_track.replace(Some(update.clone())),
            mpris::Update::Art { .. } => h.last_art.replace(Some(update.clone())),
        };
        h.each(|i| i.on_music(update.clone()));
    });
    let h = hub.clone();
    forward(lyrics_rx, move |update: lyrics::Update| {
        h.each(|i| i.on_lyrics(update.clone()));
    });
    let h = hub.clone();
    // Charging and Bluetooth peeks are always on.
    forward(power_rx, move |update| {
        let peek = power_peek(update);
        h.each(|i| i.on_peek(peek.clone()));
    });
    let h = hub.clone();
    forward(bt_rx, move |device| {
        let peek = bluetooth_peek(device);
        h.each(|i| i.on_peek(peek.clone()));
    });
    let h = hub.clone();
    forward(claude_rx, move |incoming| h.on_claude(incoming));
    let h = hub.clone();
    forward(answer_rx, move |reply| h.answer_claude(reply));
    let h = hub.clone();
    forward(translate_rx, move |on| h.set_translate(on));
    let h = hub.clone();
    forward(dnd_rx, move |on| h.set_dnd(on));
    forward(settings_rx, move |()| hub.open_settings());
}

/// Runs `f` on the UI thread for every message from a service thread.
fn forward<T: 'static>(rx: async_channel::Receiver<T>, f: impl Fn(T) + 'static) {
    glib::MainContext::default().spawn_local(async move {
        while let Ok(msg) = rx.recv().await {
            f(msg);
        }
    });
}

impl Hub {
    fn on_claude(self: &Rc<Self>, incoming: claude::Incoming) {
        if let claude::Request::CommandDone {
            command,
            status,
            seconds,
            project,
        } = &incoming.request
        {
            if self.config.borrow().features.command_peek {
                let peek = command_peek(command, *status, *seconds, project);
                self.each(|i| i.on_peek(peek.clone()));
            }
            return;
        }
        // Switched off: dropping the request (no reply) leaves Claude Code
        // to show its own prompt.
        let config = self.config.borrow();
        if config.paused || !config.features.claude {
            return;
        }
        drop(config);
        match &incoming.request {
            claude::Request::Finished { message, project } => {
                let peek = claude_peek("Claude finished", message, project);
                self.each(|i| i.on_peek(peek.clone()));
            }
            claude::Request::Waiting { message, project } => {
                let peek = claude_peek("Claude is waiting for you", message, project);
                self.each(|i| i.on_peek(peek.clone()));
            }
            claude::Request::CommandDone { .. } => {}
            _ => {
                self.claude_queue.borrow_mut().push_back(incoming);
                if self.claude_current.borrow().is_none() {
                    self.next_claude();
                }
            }
        }
    }

    /// Puts the next queued prompt on every island.
    fn next_claude(self: &Rc<Self>) {
        let Some(incoming) = self.claude_queue.borrow_mut().pop_front() else {
            self.each(|i| i.on_claude(None));
            return;
        };
        // The helper may have given up while queued (user answered in the
        // terminal); then the reply channel is closed and there's no point.
        if incoming.reply.is_closed() {
            self.next_claude();
            return;
        }
        let request = incoming.request.clone();
        let weak = Rc::downgrade(self);
        let timeout = glib::timeout_add_local_once(claude::ANSWER_TIMEOUT, move || {
            if let Some(hub) = weak.upgrade() {
                // Unanswered: drop it without a reply.
                if let Some((_, _)) = hub.claude_current.take() {
                    hub.next_claude();
                }
            }
        });
        self.claude_current.replace(Some((incoming, timeout)));
        self.each(|i| i.on_claude(Some(&request)));
        self.watch_claude();
    }

    /// While a prompt is up, notices when its hook went away (answered in
    /// the terminal or VS Code) and moves on. Runs only during a prompt.
    fn watch_claude(self: &Rc<Self>) {
        if let Some(old) = self.claude_watch.take() {
            old.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(400), move || {
            let Some(hub) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let gone = match hub.claude_current.borrow().as_ref() {
                Some((incoming, _)) => incoming.reply.is_closed(),
                None => {
                    hub.claude_watch.take();
                    return glib::ControlFlow::Break;
                }
            };
            if gone {
                hub.claude_watch.take();
                if let Some((_, timeout)) = hub.claude_current.take() {
                    timeout.remove();
                }
                hub.next_claude();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.claude_watch.replace(Some(id));
    }

    fn answer_claude(self: &Rc<Self>, reply: claude::Reply) {
        let Some((incoming, timeout)) = self.claude_current.take() else {
            return;
        };
        timeout.remove();
        let _ = incoming.reply.try_send(reply);
        self.next_claude();
    }

    /// Arms one timer for the next meeting reminder; it re-arms itself.
    fn schedule_reminder(self: &Rc<Self>) {
        if let Some(id) = self.reminder.take() {
            id.remove();
        }
        let before = i64::from(self.config.borrow().calendar.remind_before_min) * 60;
        let now = glib::DateTime::now_local().map_or(0, |t| t.to_unix());
        let next = self
            .meetings
            .borrow()
            .iter()
            .filter(|m| m.start > now && !self.reminded.borrow().contains(&m.id))
            .map(|m| (m.start - before).max(now))
            .min();
        let Some(at) = next else { return };
        let wait = u32::try_from((at - now).max(1)).unwrap_or(u32::MAX);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local_once(wait, move || {
            if let Some(hub) = weak.upgrade() {
                hub.reminder.take();
                hub.remind_due();
                hub.schedule_reminder();
            }
        });
        self.reminder.replace(Some(id));
    }

    fn remind_due(&self) {
        let before = i64::from(self.config.borrow().calendar.remind_before_min) * 60;
        let now = glib::DateTime::now_local().map_or(0, |t| t.to_unix());
        let due: Vec<calendar::Meeting> = self
            .meetings
            .borrow()
            .iter()
            .filter(|m| m.start > now && m.start - before <= now)
            .filter(|m| !self.reminded.borrow().contains(&m.id))
            .cloned()
            .collect();
        for m in due {
            self.reminded.borrow_mut().insert(m.id.clone());
            if !self.config.borrow().features.calendar {
                continue;
            }
            let peek = Peek {
                kind: Kind::Meeting,
                title: m.title.clone(),
                subtitle: format!(
                    "{} · {}",
                    crate::cards::calendar::relative(m.start, m.end, now),
                    crate::cards::calendar::clock(m.start)
                ),
                percent: None,
            };
            self.each(|i| i.on_peek(peek.clone()));
        }
    }

    fn open_settings(self: &Rc<Self>) {
        if let Some(window) = self.settings.borrow().as_ref().and_then(|w| w.upgrade()) {
            window.present();
            return;
        }
        let weak = Rc::downgrade(self);
        let window = settings::open(&self.app, self.config.borrow().clone(), move |new| {
            if let Some(hub) = weak.upgrade() {
                hub.apply_settings(new);
            }
        });
        self.settings.replace(Some(window.downgrade()));
    }

    /// Looks for a newer release; a peek says so, once per version. The
    /// Settings window does its own checks (and the installing).
    fn check_updates(self: &Rc<Self>) {
        if self.config.borrow().no_update_checks || crate::preview::active() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let status = gio::spawn_blocking(crate::update::check).await;
            let (Some(hub), Ok(crate::update::Status::Available(r))) = (weak.upgrade(), status)
            else {
                return;
            };
            if hub.update_told.replace(Some(r.version.clone())).as_ref() == Some(&r.version) {
                return;
            }
            let peek = Peek {
                kind: Kind::Notification { icon: None },
                title: format!("Atoll {} is available", r.version),
                subtitle: "Update from Atoll's Settings".into(),
                percent: None,
            };
            hub.each(|i| i.on_peek(peek.clone()));
        });
    }

    /// From the Do Not Disturb tile. Saved directly (Settings doesn't show
    /// it, so its copy may be stale).
    fn set_dnd(self: &Rc<Self>, on: bool) {
        let (before, after) = {
            let mut c = self.config.borrow_mut();
            let before = c.hides_banners();
            c.do_not_disturb = on;
            (before, c.hides_banners())
        };
        self.config.borrow().save();
        if before != after {
            if after {
                banners::hide();
            } else {
                banners::restore();
            }
        }
        self.each(|i| i.set_dnd(on));
    }

    /// Applies edited settings live and saves them.
    /// From the lyrics panel's button. Saved without rebuilding the islands
    /// (that would close the open card).
    fn set_translate(self: &Rc<Self>, on: bool) {
        let config = {
            let mut c = self.config.borrow_mut();
            c.features.translate_lyrics = on;
            c.clone()
        };
        config.save();
        self.deps.borrow_mut().features.translate_lyrics = on;
        self.each(|i| i.set_translate(on));
    }

    fn apply_settings(self: &Rc<Self>, new: Config) {
        let mut new = new.sanitized();
        // Not in the Settings window (its copy may be stale): keep the
        // lyrics button's choice.
        new.features.translate_lyrics = self.config.borrow().features.translate_lyrics;
        new.do_not_disturb = self.config.borrow().do_not_disturb;
        let old = self.config.replace(new.clone());
        new.save();
        if format!("{:?}", old.theme) != format!("{:?}", new.theme) {
            theme::apply(&new.theme);
            // Cairo-drawn parts read the palette when they draw.
            self.each(|i| redraw_all(i.root.upcast_ref()));
        }
        if format!("{:?}", old.water) != format!("{:?}", new.water) {
            if let Some(w) = self.water.borrow().clone() {
                w.set_config(new.water.clone());
            }
        }
        if old.autostart != new.autostart {
            autostart::apply(new.autostart);
        }
        if old.hides_banners() != new.hides_banners() {
            if new.hides_banners() {
                banners::hide();
            } else {
                banners::restore();
            }
        }
        if old.paused != new.paused || old.show_on != new.show_on {
            tracing::info!("atoll {}", if new.paused { "off" } else { "on" });
            self.rebuild_islands();
        }
        if format!("{:?}", old.features) != format!("{:?}", new.features) {
            self.deps.borrow_mut().features = new.features.clone();
            self.rebuild_islands();
        }
    }

    /// Features shape which cards an island has, so islands are recreated
    /// with the new set (their windows and X threads go with them).
    fn rebuild_islands(self: &Rc<Self>) {
        let old: Vec<_> = self.islands.borrow_mut().drain().collect();
        for (_, (_, window)) in old {
            window.destroy();
        }
        if let Some(display) = gdk::Display::default() {
            self.reconcile(&display.monitors());
        }
    }

    fn each(&self, f: impl Fn(&Rc<Island>)) {
        // Clone the list first: an update may not re-enter the map, but a
        // handler must never hold the borrow across GTK callbacks.
        let islands: Vec<Rc<Island>> = self
            .islands
            .borrow()
            .values()
            .map(|(i, _)| i.clone())
            .collect();
        for island in &islands {
            f(island);
        }
    }

    fn schedule_reconcile(self: &Rc<Self>, list: &gio::ListModel) {
        if let Some(id) = self.settle.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let list = list.clone();
        let id = glib::timeout_add_local_once(MONITORS_SETTLE, move || {
            if let Some(hub) = weak.upgrade() {
                hub.settle.take();
                hub.reconcile(&list);
            }
        });
        self.settle.replace(Some(id));
    }

    /// Makes the set of islands match the connected monitors.
    fn reconcile(self: &Rc<Self>, list: &gio::ListModel) {
        // Switched off: every island goes, as if no monitor were there.
        let paused = self.config.borrow().paused;
        let connectors: Vec<String> = (0..list.n_items())
            .filter(|_| !paused)
            .filter_map(|i| list.item(i).and_downcast::<gdk::Monitor>())
            .filter_map(|m| m.connector().map(|c| c.to_string()))
            // A preview renders one island.
            .take(if crate::preview::active() {
                1
            } else {
                usize::MAX
            })
            .collect();
        let primary = window::primary_connector();
        let connectors = if crate::preview::active() {
            connectors
        } else {
            self.config
                .borrow()
                .show_on
                .pick(&connectors, primary.as_deref())
        };

        let gone: Vec<String> = self
            .islands
            .borrow()
            .keys()
            .filter(|k| !connectors.contains(k))
            .cloned()
            .collect();
        for name in gone {
            if let Some((_, window)) = self.islands.borrow_mut().remove(&name) {
                tracing::info!("monitor {name} removed");
                window.destroy();
            }
        }

        for name in connectors {
            if self.islands.borrow().contains_key(&name) {
                continue;
            }
            if let Some(entry) = self.spawn_island(&name) {
                tracing::info!("monitor {name} added");
                self.islands.borrow_mut().insert(name, entry);
            }
        }
    }

    fn spawn_island(&self, connector: &str) -> Option<(Rc<Island>, ApplicationWindow)> {
        let hit: island::HitRect = Arc::new(Mutex::new((0, 0, 0, 0)));
        let island = Island::new(hit.clone(), self.deps.borrow().clone());
        let (hover_tx, hover_rx) = async_channel::unbounded();
        let window = window::show(&self.app, &island.root, connector.to_owned(), hit, hover_tx)?;

        if let Some((incoming, _)) = self.claude_current.borrow().as_ref() {
            island.on_claude(Some(&incoming.request));
        }
        if let Some(view) = self.water.borrow().as_ref().and_then(|w| w.view()) {
            island.on_water(&view);
        }
        if let Some(t) = self.todo.borrow().as_ref() {
            island.on_todo(&t.view());
        }
        if let Some(s) = self.shelf.borrow().as_ref() {
            island.on_shelf(&s.items());
        }
        island.on_notifications(&self.notifications.borrow());
        island.set_dnd(self.config.borrow().do_not_disturb);

        // Catch up on what's already playing.
        for update in [&self.last_track, &self.last_art] {
            if let Some(u) = update.borrow().clone() {
                island.on_music(u);
            }
        }
        let i = island.clone();
        if let Ok(page) = std::env::var("ATOLL_SHOW") {
            let i = island.clone();
            // After the first frame, so the expand animation has a clock.
            glib::timeout_add_local_once(Duration::from_millis(600), move || {
                i.show_for_debugging(&page);
            });
            if let Some(path) = crate::preview::shot_path() {
                let i = island.clone();
                // Long enough for the expand animation and the services'
                // first answers (track, cover, lyrics).
                let wait = std::env::var("ATOLL_SHOT_DELAY_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(3500);
                let tries = std::cell::Cell::new(0);
                glib::timeout_add_local(Duration::from_millis(wait), move || {
                    tries.set(tries.get() + 1);
                    if !i.save_png(&path) && tries.get() < 20 {
                        return glib::ControlFlow::Continue;
                    }
                    if let Some(app) = gio::Application::default() {
                        app.quit();
                    }
                    glib::ControlFlow::Break
                });
            }
        }
        // Ends by itself when the window's X thread exits and drops the sender.
        // A preview sits off screen: the real pointer and fullscreen state
        // say nothing about it.
        if !crate::preview::active() {
            forward(hover_rx, move |event| i.on_pointer(event));
        }
        Some((island, window))
    }
}

fn power_peek(update: upower::Update) -> Peek {
    match update {
        upower::Update::Plugged { percent } => Peek {
            kind: Kind::Charging,
            title: "Charging".into(),
            subtitle: "Charger connected".into(),
            percent: Some(percent),
        },
        upower::Update::Unplugged { percent } => Peek {
            kind: Kind::Unplugged,
            title: "On battery".into(),
            subtitle: "Charger disconnected".into(),
            percent: Some(percent),
        },
    }
}

fn bluetooth_peek(device: bluez::Connected) -> Peek {
    let subtitle = match device.battery {
        Some(b) => format!("Connected · {b}%"),
        None => "Connected".into(),
    };
    Peek {
        kind: Kind::Bluetooth {
            audio: device.audio,
        },
        title: device.name,
        subtitle,
        percent: device.battery.map(f64::from),
    }
}

/// Queues a redraw on every widget in the tree; `queue_draw` on a parent
/// doesn't repaint children's own drawing functions.
fn redraw_all(widget: &gtk::Widget) {
    widget.queue_draw();
    let mut child = widget.first_child();
    while let Some(c) = child {
        redraw_all(&c);
        child = c.next_sibling();
    }
}

fn claude_peek(title: &str, message: &str, project: &str) -> Peek {
    let subtitle = match (project.is_empty(), message.is_empty()) {
        (false, false) => format!("{project} · {message}"),
        (false, true) => project.to_owned(),
        _ => message.to_owned(),
    };
    Peek {
        kind: Kind::Claude,
        title: title.into(),
        subtitle,
        percent: None,
    }
}

fn command_peek(command: &str, status: i32, seconds: u64, project: &str) -> Peek {
    let took = match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    };
    let outcome = if status == 0 {
        format!("Done in {took}")
    } else {
        format!("Failed (exit {status}) after {took}")
    };
    let subtitle = if project.is_empty() {
        outcome
    } else {
        format!("{outcome} · {project}")
    };
    Peek {
        kind: Kind::Command { ok: status == 0 },
        title: command.to_owned(),
        subtitle,
        percent: None,
    }
}
