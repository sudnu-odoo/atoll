//! Settings window: a normal desktop window (unlike the island) where
//! features, theme and water options are edited. Every change is handed to
//! `on_change` immediately; there's no Save button.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, Align, Application, Label, Orientation};

use crate::config::{self, Config};

/// Theme presets: (name, accent, secondary). Dracula first, the default.
const PRESETS: &[(&str, &str, &str)] = &[
    ("Dracula", "#FF8FB1", "#C9B8FF"),
    ("Cyan", "#8BE9FD", "#BD93F9"),
    ("Mint", "#50FA7B", "#8BE9FD"),
    ("Sunset", "#FFB86C", "#FF79C6"),
];

type OnChange = Rc<dyn Fn(Config)>;
/// Rebuilds the card-order list; set once the list exists.
type Refresh = Rc<RefCell<Option<Box<dyn Fn()>>>>;

/// Builds and shows the window. `config` is the current state.
pub fn open(
    app: &Application,
    config: Config,
    on_change: impl Fn(Config) + 'static,
) -> gtk::Window {
    let state = Rc::new(RefCell::new(config));
    // The order list shows only switched-on cards, so it is rebuilt after
    // every change (set once the list exists, below).
    let refresh_order: Refresh = Rc::default();
    let hook = refresh_order.clone();
    let on_change: OnChange = Rc::new(move |c| {
        on_change(c);
        let hook = hook.clone();
        // From idle: the change may come from inside the list itself.
        glib::idle_add_local_once(move || {
            if let Some(f) = hook.borrow().as_ref() {
                f();
            }
        });
    });

    let content = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .css_classes(["settings-page"])
        .spacing(22)
        .build();
    content.append(&section(
        "General",
        &[
            switch(
                &state,
                &on_change,
                "Active",
                "Show the notch. When off, Atoll stays out of the way until switched back on here",
                |c| !c.paused,
                |c, v| c.paused = !v,
            ),
            switch(
                &state,
                &on_change,
                "Start at login",
                "Open Atoll automatically when you log in",
                |c| c.autostart,
                |c, v| c.autostart = v,
            ),
            show_on_row(&state, &on_change),
            bridge_row(),
            updates_row(),
            switch(
                &state,
                &on_change,
                "Check for updates automatically",
                "Once a day; a peek says when a new version is out",
                |c| !c.no_update_checks,
                |c, v| c.no_update_checks = !v,
            ),
        ],
    ));
    content.append(&features_section(&state, &on_change));
    content.append(&order_section(&state, &on_change, &refresh_order));
    content.append(&theme_section(&state, &on_change));
    content.append(
        &Label::builder()
            .label("Changes apply right away and are saved to ~/.config/atoll/config.json")
            .css_classes(["settings-hint"])
            .wrap(true)
            .xalign(0.0)
            .build(),
    );

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content)
        .build();
    let window = gtk::Window::builder()
        .application(app)
        .title("Atoll Settings")
        .css_classes(["settings"])
        .default_width(480)
        .default_height(680)
        .child(&scroller)
        .build();
    window.present();
    window
}

/// Applies `edit` to the shared config, then reports the new config.
fn changer(
    state: &Rc<RefCell<Config>>,
    on_change: &OnChange,
    edit: impl Fn(&mut Config) + 'static,
) -> impl Fn() + 'static {
    let (state, on_change) = (state.clone(), on_change.clone());
    move || {
        edit(&mut state.borrow_mut());
        let snapshot = state.borrow().clone();
        on_change(snapshot);
    }
}

fn section(title: &str, rows: &[gtk::Widget]) -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(6)
        .build();
    b.append(
        &Label::builder()
            .label(title.to_uppercase())
            .css_classes(["settings-section"])
            .xalign(0.0)
            .build(),
    );
    let card = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .css_classes(["settings-card"])
        .build();
    for r in rows {
        card.append(r);
    }
    b.append(&card);
    b
}

/// This version, a check for a newer one, what's new in it, and Update
/// (download + the system's password prompt + restart).
fn updates_row() -> gtk::Widget {
    use crate::update::{self, Status};
    let button = gtk::Button::builder()
        .label("Check now")
        .valign(Align::Center)
        .build();
    let widget = row(
        "Updates",
        &format!("Atoll {} · not checked yet", update::VERSION),
        &button,
    );
    let subtitle = widget
        .first_child()
        .and_then(|text| text.last_child())
        .and_downcast::<Label>();
    // What changed, under the row, once a newer version is found.
    let notes = Label::builder()
        .css_classes(["settings-hint"])
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .visible(false)
        .build();
    let column = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(6)
        .build();
    column.append(&widget);
    column.append(&notes);

    let found: Rc<RefCell<Option<update::Release>>> = Rc::default();
    let say = move |text: &str| {
        if let Some(l) = &subtitle {
            l.set_label(text);
        }
    };
    let say = Rc::new(say);
    button.connect_clicked(move |button| {
        let (button, notes, found, say) =
            (button.clone(), notes.clone(), found.clone(), say.clone());
        button.set_sensitive(false);
        let pending = found.borrow().clone();
        glib::spawn_future_local(async move {
            match pending {
                // Found earlier: this click installs it.
                Some(release) => {
                    say(&format!("Installing Atoll {}…", release.version));
                    let r = release.clone();
                    match gio::spawn_blocking(move || update::install(&r)).await {
                        Ok(Ok(())) => {
                            say("Installed · restarting Atoll");
                            update::restart_after_quit();
                            if let Some(app) = gio::Application::default() {
                                app.quit();
                            }
                        }
                        Ok(Err(e)) => {
                            say(&format!("Update {}: {e}", release.version));
                            button.set_sensitive(true);
                        }
                        Err(_) => button.set_sensitive(true),
                    }
                }
                None => {
                    say("Checking…");
                    match gio::spawn_blocking(update::check).await {
                        Ok(Status::UpToDate) => {
                            say(&format!("Atoll {} · up to date", update::VERSION));
                            button.set_label("Check again");
                        }
                        Ok(Status::Available(r)) => {
                            say(&format!(
                                "Atoll {} available (you have {})",
                                r.version,
                                update::VERSION
                            ));
                            notes.set_label(&if r.notes.is_empty() {
                                "No notes for this release.".to_owned()
                            } else {
                                format!("What's new in {}:\n{}", r.version, r.notes)
                            });
                            notes.set_visible(true);
                            button.set_label("Update");
                            button.add_css_class("suggested-action");
                            found.replace(Some(r));
                        }
                        Ok(Status::Failed(e)) => {
                            tracing::warn!("update check failed: {e}");
                            say("Couldn't check (offline?)");
                        }
                        Err(_) => say("Couldn't check"),
                    }
                    button.set_sensitive(true);
                }
            }
        });
    });
    column.upcast()
}

/// Which monitors get a notch: the main one (with GNOME's top bar), all, or
/// one of those connected now.
fn show_on_row(state: &Rc<RefCell<Config>>, on_change: &OnChange) -> gtk::Widget {
    use crate::config::ShowOn;
    let mut choices = vec![ShowOn::Primary, ShowOn::All];
    let mut names = vec!["Main monitor".to_owned(), "All monitors".to_owned()];
    if let Some(display) = gdk::Display::default() {
        let list = display.monitors();
        for i in 0..list.n_items() {
            let Some(m) = list.item(i).and_downcast::<gdk::Monitor>() else {
                continue;
            };
            let Some(connector) = m.connector() else {
                continue;
            };
            let model = [m.manufacturer(), m.model()]
                .into_iter()
                .flatten()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            let g = m.geometry();
            names.push(if model.is_empty() {
                format!("{connector} ({}×{})", g.width(), g.height())
            } else {
                format!("{model} ({connector})")
            });
            choices.push(ShowOn::Monitor(connector.to_string()));
        }
    }
    // A saved monitor that isn't connected now still shows as chosen.
    let current = state.borrow().show_on.clone();
    if !choices.contains(&current) {
        if let ShowOn::Monitor(name) = &current {
            names.push(format!("{name} (not connected)"));
            choices.push(current.clone());
        }
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let dropdown = gtk::DropDown::from_strings(&refs);
    dropdown.set_selected(choices.iter().position(|c| *c == current).unwrap_or(0) as u32);
    let (st, oc) = (state.clone(), on_change.clone());
    dropdown.connect_selected_notify(move |d| {
        if let Some(choice) = choices.get(d.selected() as usize).cloned() {
            changer(&st, &oc, move |c| c.show_on = choice.clone())();
        }
    });
    row(
        "Show on",
        "Main monitor is the one with the GNOME top bar",
        &dropdown,
    )
}

/// The Atoll Bridge extension: its live state, and a switch that turns it
/// on or off for this user (only ever when the user flips it).
fn bridge_row() -> gtk::Widget {
    use crate::bridge::{self, Status};
    let sw = gtk::Switch::builder().sensitive(false).build();
    let widget = row("Atoll Bridge", "Checking…", &sw);
    let subtitle = widget
        .first_child()
        .and_then(|text| text.last_child())
        .and_downcast::<Label>();
    // Set while the switch is moved to match GNOME's answer, so that
    // move isn't taken for the user's click.
    let syncing = Rc::new(std::cell::Cell::new(false));
    let show: Rc<dyn Fn(Status)> = {
        let (sw, syncing) = (sw.clone(), syncing.clone());
        Rc::new(move |status: Status| {
            if let Some(l) = &subtitle {
                l.set_label(&format!(
                    "Open notifications like GNOME's banners. {}",
                    status.describe()
                ));
            }
            syncing.set(true);
            sw.set_active(status == Status::On);
            syncing.set(false);
            sw.set_sensitive(matches!(status, Status::On | Status::Off | Status::Error));
        })
    };
    let first = show.clone();
    glib::spawn_future_local(async move { first(bridge::status().await) });
    sw.connect_active_notify(move |sw| {
        if syncing.get() {
            return;
        }
        let (on, show) = (sw.is_active(), show.clone());
        sw.set_sensitive(false);
        glib::spawn_future_local(async move { show(bridge::set(on).await) });
    });
    widget
}

fn row(title: &str, subtitle: &str, control: &impl IsA<gtk::Widget>) -> gtk::Widget {
    let text = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .hexpand(true)
        .valign(Align::Center)
        .spacing(2)
        .build();
    text.append(
        &Label::builder()
            .label(title)
            .css_classes(["settings-title"])
            .xalign(0.0)
            .build(),
    );
    if !subtitle.is_empty() {
        text.append(
            &Label::builder()
                .label(subtitle)
                .css_classes(["settings-subtitle"])
                .xalign(0.0)
                .wrap(true)
                .build(),
        );
    }
    let r = gtk::Box::builder()
        .css_classes(["settings-row"])
        .spacing(16)
        .build();
    r.append(&text);
    control.set_valign(Align::Center);
    r.append(control);
    r.upcast()
}

fn switch(
    state: &Rc<RefCell<Config>>,
    on_change: &OnChange,
    title: &str,
    subtitle: &str,
    get: fn(&Config) -> bool,
    set: fn(&mut Config, bool),
) -> gtk::Widget {
    switch_with(state, on_change, title, subtitle, get, set).0
}

fn switch_with(
    state: &Rc<RefCell<Config>>,
    on_change: &OnChange,
    title: &str,
    subtitle: &str,
    get: fn(&Config) -> bool,
    set: fn(&mut Config, bool),
) -> (gtk::Widget, gtk::Switch) {
    let sw = gtk::Switch::builder().active(get(&state.borrow())).build();
    let (st, oc) = (state.clone(), on_change.clone());
    sw.connect_active_notify(move |sw| {
        let on = sw.is_active();
        changer(&st, &oc, move |c| set(c, on))();
    });
    (row(title, subtitle, &sw), sw)
}

/// A feature switch with its own options underneath, which slide open only
/// while the feature is on; options of a disabled feature take no space.
fn feature(main: (gtk::Widget, gtk::Switch), options: &[gtk::Widget]) -> gtk::Widget {
    let (row, sw) = main;
    let sub = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .css_classes(["settings-sub"])
        .build();
    for o in options {
        sub.append(o);
    }
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(220)
        .child(&sub)
        .build();
    sw.bind_property("active", &revealer, "reveal-child")
        .sync_create()
        .build();
    let group = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .build();
    group.append(&row);
    group.append(&revealer);
    group.upcast()
}

fn features_section(state: &Rc<RefCell<Config>>, on_change: &OnChange) -> gtk::Box {
    let music = switch_with(
        state,
        on_change,
        "Music",
        "Now playing, controls and the timeline",
        |c| c.features.music,
        |c, v| c.features.music = v,
    );
    let lyrics = switch(
        state,
        on_change,
        "Lyrics",
        "Synced lyrics from LRCLIB (sends title and artist)",
        |c| c.features.lyrics,
        |c, v| c.features.lyrics = v,
    );
    let water = switch_with(
        state,
        on_change,
        "Water",
        "Water card and drink reminders",
        |c| c.features.water,
        |c, v| c.features.water = v,
    );
    let rows = [
        feature(music, &[lyrics]),
        switch(
            state,
            on_change,
            "Eyes",
            "Sleepy or excited eyes taking turns with the clock",
            |c| c.features.eyes,
            |c, v| c.features.eyes = v,
        ),
        feature(water, &water_options(state, on_change)),
        switch(
            state,
            on_change,
            "To-do",
            "Today's tasks; tick them off from the notch",
            |c| c.features.todo,
            |c, v| c.features.todo = v,
        ),
        switch(
            state,
            on_change,
            "Shelf",
            "Drop files on the notch, drag them out where you need them",
            |c| c.features.shelf,
            |c, v| c.features.shelf = v,
        ),
        switch(
            state,
            on_change,
            "Controls",
            "Wi-Fi, Bluetooth, volume, brightness, power mode, lock",
            |c| c.features.controls,
            |c, v| c.features.controls = v,
        ),
        switch(
            state,
            on_change,
            "Notifications",
            "Show notifications in Atoll and hide GNOME's own banners (given back when off)",
            |c| c.features.notifications,
            |c, v| c.features.notifications = v,
        ),
        feature(
            switch_with(
                state,
                on_change,
                "Calendar",
                "Next meetings from Google Calendar (via Settings → Online Accounts)",
                |c| c.features.calendar,
                |c, v| c.features.calendar = v,
            ),
            &[row(
                "Remind before",
                "Minutes before a meeting to show a peek",
                &{
                    let s = gtk::SpinButton::with_range(1.0, 60.0, 1.0);
                    s.set_value(f64::from(state.borrow().calendar.remind_before_min));
                    let (st, oc) = (state.clone(), on_change.clone());
                    s.connect_value_changed(move |s| {
                        let v = s.value() as u32;
                        changer(&st, &oc, move |c| c.calendar.remind_before_min = v)();
                    });
                    s
                },
            )],
        ),
        feature(
            switch_with(
                state,
                on_change,
                "Command finished",
                "Peek when a long terminal command ends (not ones you stop yourself)",
                |c| c.features.command_peek,
                |c, v| c.features.command_peek = v,
            ),
            &[row(
                "Minimum duration",
                "Seconds a command must run to count",
                &{
                    let s = gtk::SpinButton::with_range(3.0, 600.0, 1.0);
                    s.set_value(f64::from(state.borrow().commands.min_seconds));
                    let (st, oc) = (state.clone(), on_change.clone());
                    s.connect_value_changed(move |s| {
                        let v = s.value() as u32;
                        changer(&st, &oc, move |c| c.commands.min_seconds = v)();
                    });
                    s
                },
            )],
        ),
        switch(
            state,
            on_change,
            "Claude",
            "Answer Claude Code prompts from the notch (needs the hooks installed)",
            |c| c.features.claude,
            |c, v| c.features.claude = v,
        ),
    ];
    section("Features", &rows)
}

fn rgba(hex: &str) -> gdk::RGBA {
    gdk::RGBA::parse(hex).unwrap_or(gdk::RGBA::WHITE)
}

fn to_hex(c: &gdk::RGBA) -> String {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        ch(c.red()),
        ch(c.green()),
        ch(c.blue())
    )
}

fn color_button(
    state: &Rc<RefCell<Config>>,
    on_change: &OnChange,
    get: fn(&Config) -> String,
    set: fn(&mut Config, String),
) -> gtk::ColorDialogButton {
    let dialog = gtk::ColorDialog::builder().with_alpha(false).build();
    let button = gtk::ColorDialogButton::builder()
        .dialog(&dialog)
        .rgba(&rgba(&get(&state.borrow())))
        .build();
    let (state, on_change) = (state.clone(), on_change.clone());
    button.connect_rgba_notify(move |b| {
        let hex = to_hex(&b.rgba());
        // Presets set the button too; don't report a change that isn't one.
        if get(&state.borrow()).eq_ignore_ascii_case(&hex) {
            return;
        }
        changer(&state, &on_change, move |c| set(c, hex.clone()))();
    });
    button
}

fn card_label(name: &str) -> &'static str {
    match name {
        "music" => "Music / Clock",
        "controls" => "Controls",
        "notifications" => "Notifications",
        "calendar" => "Calendar",
        "water" => "Water",
        "todo" => "To-do",
        "shelf" => "Shelf",
        _ => "Card",
    }
}

fn card_enabled(f: &crate::config::Features, name: &str) -> bool {
    match name {
        "music" => true, // the clock card when music is off or idle
        "controls" => f.controls,
        "notifications" => f.notifications,
        "calendar" => f.calendar,
        "water" => f.water,
        "todo" => f.todo,
        "shelf" => f.shelf,
        _ => false,
    }
}

/// Which card comes first when the notch opens, and the order after it.
fn order_section(state: &Rc<RefCell<Config>>, on_change: &OnChange, refresh: &Refresh) -> gtk::Box {
    let list = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .build();
    fill_order(&list, state, on_change);
    let (l, s, c) = (list.clone(), state.clone(), on_change.clone());
    refresh.replace(Some(Box::new(move || fill_order(&l, &s, &c))));
    let wrapper = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(6)
        .build();
    wrapper.append(
        &Label::builder()
            .label("CARD ORDER")
            .css_classes(["settings-section"])
            .xalign(0.0)
            .build(),
    );
    let card = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .css_classes(["settings-card"])
        .build();
    card.append(&list);
    wrapper.append(&card);
    wrapper.append(
        &Label::builder()
            .label("The first card opens when you hover the island")
            .css_classes(["settings-hint"])
            .xalign(0.0)
            .build(),
    );
    wrapper
}

/// (Re)builds the order rows; moving one rebuilds the list.
fn fill_order(list: &gtk::Box, state: &Rc<RefCell<Config>>, on_change: &OnChange) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    // Only cards that are switched on; `at[i]` is where shown row i sits in
    // the full order, so moving rows keeps hidden cards' places.
    let (order, at): (Vec<String>, Vec<usize>) = {
        let c = state.borrow();
        let f = &c.features;
        f.order
            .iter()
            .enumerate()
            .filter(|(_, name)| card_enabled(f, name))
            .map(|(i, name)| (name.clone(), i))
            .unzip()
    };
    let last = order.len().saturating_sub(1);
    for (i, name) in order.iter().enumerate() {
        let arrow = |icon: &str, tip: &str, enabled: bool, to: usize| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .css_classes(["flat", "order-arrow"])
                .sensitive(enabled)
                .build();
            b.set_cursor_from_name(Some("pointer"));
            let (list, state, on_change) = (list.clone(), state.clone(), on_change.clone());
            let (from, to) = (at[i], at[to]);
            b.connect_clicked(move |_| {
                state.borrow_mut().features.order.swap(from, to);
                let snapshot = state.borrow().clone();
                on_change(snapshot);
                fill_order(&list, &state, &on_change);
            });
            b
        };
        let up = arrow("go-up-symbolic", "Move up", i > 0, i.saturating_sub(1));
        let down = arrow("go-down-symbolic", "Move down", i < last, (i + 1).min(last));
        let buttons = gtk::Box::builder().spacing(4).build();
        buttons.append(&up);
        buttons.append(&down);
        let position = format!("{}", i + 1);
        let r = gtk::Box::builder()
            .css_classes(["settings-row"])
            .spacing(14)
            .build();
        r.append(
            &Label::builder()
                .label(position)
                .css_classes(["order-number"])
                .build(),
        );
        r.append(
            &Label::builder()
                .label(card_label(name))
                .css_classes(["settings-title"])
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        r.append(&buttons);
        list.append(&r);
    }
}

fn theme_section(state: &Rc<RefCell<Config>>, on_change: &OnChange) -> gtk::Box {
    let accent = color_button(
        state,
        on_change,
        |c| c.theme.accent.clone(),
        |c, v| c.theme.accent = v,
    );
    let secondary = color_button(
        state,
        on_change,
        |c| c.theme.secondary.clone(),
        |c, v| c.theme.secondary = v,
    );

    let presets = gtk::Box::builder().spacing(8).build();
    for (name, a, s) in PRESETS {
        let swatch = gtk::Button::builder()
            .label(*name)
            .css_classes(["preset"])
            .build();
        swatch.set_cursor_from_name(Some("pointer"));
        let (state, on_change) = (state.clone(), on_change.clone());
        let (accent, secondary) = (accent.clone(), secondary.clone());
        swatch.connect_clicked(move |_| {
            {
                let mut c = state.borrow_mut();
                c.theme.accent = (*a).into();
                c.theme.secondary = (*s).into();
            }
            accent.set_rgba(&rgba(a));
            secondary.set_rgba(&rgba(s));
            let snapshot = state.borrow().clone();
            on_change(snapshot);
        });
        presets.append(&swatch);
    }

    // System (follows GNOME's light/dark style), or always one of them.
    let appearance = gtk::DropDown::from_strings(&["System", "Dark", "Light"]);
    appearance.set_selected(match state.borrow().theme.appearance {
        crate::config::Appearance::System => 0,
        crate::config::Appearance::Dark => 1,
        crate::config::Appearance::Light => 2,
    });
    let (st, oc) = (state.clone(), on_change.clone());
    appearance.connect_selected_notify(move |d| {
        let a = match d.selected() {
            1 => crate::config::Appearance::Dark,
            2 => crate::config::Appearance::Light,
            _ => crate::config::Appearance::System,
        };
        changer(&st, &oc, move |c| c.theme.appearance = a)();
    });

    let rows = [
        row(
            "Appearance",
            "Light or dark notch; System follows GNOME's style",
            &appearance,
        ),
        row("Accent", "Progress, active buttons, excited eyes", &accent),
        row("Secondary", "Labels, meters, sleepy eyes", &secondary),
        row("Presets", "", &presets),
    ];
    section("Theme", &rows)
}

fn spin(
    state: &Rc<RefCell<Config>>,
    on_change: &OnChange,
    (min, max, step): (f64, f64, f64),
    digits: u32,
    get: fn(&config::Water) -> f64,
    set: fn(&mut config::Water, f64),
) -> gtk::SpinButton {
    let s = gtk::SpinButton::with_range(min, max, step);
    s.set_digits(digits);
    s.set_value(get(&state.borrow().water));
    let (state, on_change) = (state.clone(), on_change.clone());
    s.connect_value_changed(move |s| {
        let v = s.value();
        changer(&state, &on_change, move |c| set(&mut c.water, v))();
    });
    s
}

fn water_options(state: &Rc<RefCell<Config>>, on_change: &OnChange) -> Vec<gtk::Widget> {
    let goal = spin(
        state,
        on_change,
        (0.5, 6.0, 0.25),
        2,
        |w| f64::from(w.goal_ml) / 1000.0,
        |w, v| w.goal_ml = (v * 1000.0).round() as u32,
    );
    let glass = spin(
        state,
        on_change,
        (50.0, 1000.0, 50.0),
        0,
        |w| f64::from(w.glass_ml),
        |w, v| w.glass_ml = v as u32,
    );
    let every = spin(
        state,
        on_change,
        (10.0, 240.0, 5.0),
        0,
        |w| f64::from(w.remind_every_min),
        |w, v| w.remind_every_min = v as u32,
    );
    let from = spin(
        state,
        on_change,
        (0.0, 23.0, 1.0),
        0,
        |w| f64::from(w.active_from_hour),
        |w, v| w.active_from_hour = v as u32,
    );
    let to = spin(
        state,
        on_change,
        (1.0, 24.0, 1.0),
        0,
        |w| f64::from(w.active_to_hour),
        |w, v| w.active_to_hour = v as u32,
    );
    let hours = gtk::Box::builder().spacing(6).build();
    hours.append(&from);
    hours.append(&Label::new(Some("to")));
    hours.append(&to);

    vec![
        switch(
            state,
            on_change,
            "Reminders",
            "A peek when it's been a while since the last drink",
            |c| c.water.reminders,
            |c, v| c.water.reminders = v,
        ),
        row("Daily goal", "Litres", &goal),
        row("Glass size", "ml added by one Drink click", &glass),
        row("Remind every", "Minutes after the last drink", &every),
        row("Active hours", "No reminders outside these hours", &hours),
    ]
}
