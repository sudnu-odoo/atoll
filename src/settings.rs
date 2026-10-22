//! Settings window: a normal desktop window (unlike the island) where
//! features, theme and water options are edited. Every change is handed to
//! `on_change` immediately; there's no Save button.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, Align, Application, Label, Orientation};

use crate::config::{self, Config};

/// Theme presets: (name, accent, secondary). Dracula first, the default.
const PRESETS: &[(&str, &str, &str)] = &[
    ("Dracula", "#FF8FB1", "#C9B8FF"),
    ("Cyan", "#8BE9FD", "#BD93F9"),
    ("Mint", "#50FA7B", "#8BE9FD"),
    ("Sunset", "#FFB86C", "#FF79C6"),
];

type OnChange = Rc<dyn Fn(Config)>;

/// Builds and shows the window. `config` is the current state.
pub fn open(
    app: &Application,
    config: Config,
    on_change: impl Fn(Config) + 'static,
) -> gtk::Window {
    let state = Rc::new(RefCell::new(config));
    let on_change: OnChange = Rc::new(on_change);

    let content = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .css_classes(["settings-page"])
        .spacing(22)
        .build();
    content.append(&features_section(&state, &on_change));
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
            "Bluetooth peek",
            "When a device connects",
            |c| c.features.bluetooth_peek,
            |c, v| c.features.bluetooth_peek = v,
        ),
        switch(
            state,
            on_change,
            "Charging peek",
            "When the charger is plugged or unplugged",
            |c| c.features.charging_peek,
            |c, v| c.features.charging_peek = v,
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

    let rows = [
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
