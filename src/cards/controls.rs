//! Quick settings card, like GNOME's: volume and brightness sliders on the
//! left, toggle tiles on the right. A tile's › (and the volume row's ›)
//! opens a menu in the card: Wi-Fi networks, Bluetooth devices, power modes,
//! sound outputs. Asks the controls service for fresh state whenever it's
//! shown; nothing runs while it's hidden.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib, Align, Label, Orientation};

use super::todo::Typing;
use super::{control_button, every_minute, live_clock};
use crate::services::controls::{Command, Menu, MenuItem, State};

/// Power modes in click order.
const PROFILES: [(&str, &str); 3] = [
    ("power-saver", "Power Saver"),
    ("balanced", "Balanced"),
    ("performance", "Performance"),
];

struct Tile {
    root: gtk::Box,
    icon: gtk::Image,
    subtitle: Label,
}

struct Slider {
    row: gtk::Box,
    icon: gtk::Button,
    scale: gtk::Scale,
    value: Label,
}

pub struct ControlsCard {
    pub root: gtk::Box,
    view: gtk::Stack,
    volume: Slider,
    brightness: Slider,
    wifi: Tile,
    bluetooth: Tile,
    airplane: Tile,
    power: Tile,
    lock: Tile,
    menu_title: Label,
    menu_list: gtk::Box,
    password_row: gtk::Box,
    password: gtk::PasswordEntry,
    commands: async_channel::Sender<Command>,
    on_typing: Rc<dyn Fn(Typing)>,
    state: RefCell<State>,
    /// The user is dragging a slider: don't move it from under them.
    adjusting: Cell<bool>,
    /// A picked item is connecting (Bluetooth can take seconds): further
    /// picks wait until the service reports back with a fresh list.
    busy: Rc<Cell<bool>>,
}

impl ControlsCard {
    pub fn new(
        commands: async_channel::Sender<Command>,
        on_typing: impl Fn(Typing) + 'static,
    ) -> Rc<Self> {
        // Top row sits under the GNOME bar: labels only.
        let title = gtk::Box::builder().spacing(6).build();
        title.append(
            &gtk::Image::builder()
                .icon_name("atoll-controls-symbolic")
                .css_classes(["todo-icon"])
                .build(),
        );
        title.append(
            &Label::builder()
                .label("CONTROLS")
                .css_classes(["caps", "lavender"])
                .build(),
        );
        let date = Label::builder().css_classes(["date"]).build();
        let d = date.clone();
        every_minute(move |now| {
            if let Ok(s) = now.format("%a %d %b %Y") {
                d.set_label(&s);
            }
        });
        let time = Label::builder().css_classes(["clock"]).build();
        live_clock(&time, "%H:%M:%S");
        let when = gtk::Box::builder().spacing(8).build();
        when.append(&date);
        when.append(&time);
        let top = gtk::CenterBox::builder()
            .css_classes(["card-top"])
            .start_widget(&title)
            .center_widget(&when)
            .build();

        // Main view: sliders + tiles.
        let volume = slider("atoll-volume-symbolic", "Mute", true);
        let brightness = slider("atoll-brightness-symbolic", "Brightness", false);
        let sliders = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(14)
            .width_request(176)
            .valign(Align::Center)
            .build();
        sliders.append(&volume.row);
        sliders.append(&brightness.row);

        let wifi = tile("atoll-wifi-symbolic", "Wi-Fi", true);
        let bluetooth = tile("atoll-bluetooth-symbolic", "Bluetooth", true);
        let airplane = tile("atoll-airplane-symbolic", "Airplane", false);
        let power = tile("atoll-speed-symbolic", "Power Mode", true);
        let lock = tile("atoll-lock-symbolic", "Lock", false);
        lock.subtitle.set_label("Lock screen");
        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(6)
            .column_homogeneous(true)
            .hexpand(true)
            .valign(Align::Center)
            .build();
        grid.attach(&wifi.root, 0, 0, 1, 1);
        grid.attach(&bluetooth.root, 1, 0, 1, 1);
        grid.attach(&airplane.root, 0, 1, 1, 1);
        grid.attach(&power.root, 1, 1, 1, 1);
        grid.attach(&lock.root, 0, 2, 1, 1);
        let main = gtk::Box::builder().spacing(20).build();
        main.append(&sliders);
        main.append(&grid);

        // Menu view: ‹ title, items, and a password row when needed.
        let back = control_button(&["cal-nav"], Some("atoll-left-symbolic"));
        back.set_tooltip_text(Some("Back"));
        let menu_title = Label::builder()
            .css_classes(["cal-month"])
            .xalign(0.0)
            .build();
        let header = gtk::Box::builder().spacing(8).build();
        header.append(&back);
        header.append(&menu_title);
        let menu_list = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .min_content_height(100)
            .max_content_height(100)
            .child(&menu_list)
            .build();
        let password = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .placeholder_text("Password")
            .hexpand(true)
            .css_classes(["todo-field"])
            .build();
        let join = control_button(&["pill", "todo-save"], None);
        join.set_label("Connect");
        let password_row = gtk::Box::builder().spacing(8).visible(false).build();
        password_row.append(&password);
        password_row.append(&join);
        let menu = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(6)
            .build();
        menu.append(&header);
        menu.append(&scroller);
        menu.append(&password_row);

        let view = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .transition_duration(180)
            .css_classes(["todo-body"])
            .build();
        view.add_named(&main, Some("main"));
        view.add_named(&menu, Some("menu"));

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["water-card"])
            .width_request(crate::island::EXPANDED_CONTROLS.0 as i32)
            .height_request(crate::island::EXPANDED_CONTROLS.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&top);
        root.append(&view);

        let card = Rc::new(Self {
            root,
            view,
            volume,
            brightness,
            wifi,
            bluetooth,
            airplane,
            power,
            lock,
            menu_title,
            menu_list,
            password_row,
            password,
            commands,
            on_typing: Rc::new(on_typing),
            state: RefCell::default(),
            adjusting: Cell::new(false),
            busy: Rc::default(),
        });
        card.connect(&back, &join);
        card.update(&State::default());
        card
    }

    fn send(&self, cmd: Command) {
        let _ = self.commands.try_send(cmd);
    }

    fn connect(self: &Rc<Self>, back: &gtk::Button, join: &gtk::Button) {
        // Fresh state each time the card comes on screen; leaving it closes
        // any menu, so it opens on the tiles next time.
        let weak = Rc::downgrade(self);
        self.root.connect_map(move |_| {
            if let Some(c) = weak.upgrade() {
                c.send(Command::Refresh);
            }
        });
        let weak = Rc::downgrade(self);
        self.root.connect_unmap(move |_| {
            if let Some(c) = weak.upgrade() {
                c.close_menu();
            }
        });

        let weak = Rc::downgrade(self);
        self.volume.icon.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.send(Command::ToggleMute);
            }
        });
        for (scale, is_volume) in [(&self.volume.scale, true), (&self.brightness.scale, false)] {
            // `change-value` fires for user input only, never for our own
            // updates, so refreshes can't loop back into commands.
            let weak = Rc::downgrade(self);
            scale.connect_change_value(move |_, _, v| {
                if let Some(c) = weak.upgrade() {
                    c.adjusting.set(true);
                    let v = v.clamp(0.0, 1.0);
                    c.send(if is_volume {
                        Command::SetVolume(v)
                    } else {
                        Command::SetBrightness(v)
                    });
                    let weak = Rc::downgrade(&c);
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(600),
                        move || {
                            if let Some(c) = weak.upgrade() {
                                c.adjusting.set(false);
                            }
                        },
                    );
                }
                glib::Propagation::Proceed
            });
        }

        let on_toggle = |tile: &Tile, f: fn(&State) -> Option<Command>| {
            let weak = Rc::downgrade(self);
            if let Some(b) = tile.root.first_child().and_downcast::<gtk::Button>() {
                b.connect_clicked(move |_| {
                    if let Some(c) = weak.upgrade() {
                        let cmd = f(&c.state.borrow());
                        if let Some(cmd) = cmd {
                            c.send(cmd);
                        }
                    }
                });
            }
        };
        on_toggle(&self.wifi, |s| {
            s.wifi.as_ref().map(|(on, _)| Command::SetWifi(!on))
        });
        on_toggle(&self.bluetooth, |s| {
            s.bluetooth
                .as_ref()
                .map(|(on, _)| Command::SetBluetooth(!on))
        });
        on_toggle(&self.airplane, |s| {
            s.airplane.map(|on| Command::SetAirplane(!on))
        });
        on_toggle(&self.power, |s| {
            let current = s.profile.as_deref()?;
            let i = PROFILES
                .iter()
                .position(|(id, _)| *id == current)
                .unwrap_or(0);
            Some(Command::SetProfile(
                PROFILES[(i + 1) % PROFILES.len()].0.into(),
            ))
        });
        on_toggle(&self.lock, |_| Some(Command::Lock));

        // The › on tiles and on the volume row.
        let opens = [
            (self.wifi.root.last_child(), Menu::Wifi),
            (self.bluetooth.root.last_child(), Menu::Bluetooth),
            (self.power.root.last_child(), Menu::Power),
            (self.volume.row.last_child(), Menu::Sound),
        ];
        for (widget, menu) in opens {
            let Some(b) = widget.and_downcast::<gtk::Button>() else {
                continue;
            };
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.open_menu(menu);
                }
            });
        }

        let weak = Rc::downgrade(self);
        back.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.close_menu();
            }
        });
        let submit = {
            let weak = Rc::downgrade(self);
            move || {
                let Some(c) = weak.upgrade() else { return };
                let ssid = c.state.borrow().needs_password.clone();
                if let Some(ssid) = ssid {
                    let pass = c.password.text().to_string();
                    if !pass.is_empty() {
                        c.send(Command::JoinWifi(ssid, pass));
                        c.password.set_text("");
                        c.stop_typing(true);
                    }
                }
            }
        };
        let submit = Rc::new(submit);
        let s = submit.clone();
        join.connect_clicked(move |_| s());
        let s = submit.clone();
        self.password.connect_activate(move |_| s());
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(c) = weak.upgrade() {
                    c.password_row.set_visible(false);
                    c.stop_typing(true);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.password.add_controller(keys);
    }

    /// Developer aid (`ATOLL_SHOW=controls-wifi`): open the Wi-Fi list.
    pub fn show_menu_for_debugging(&self) {
        self.open_menu(Menu::Wifi);
    }

    fn open_menu(&self, menu: Menu) {
        self.menu_title.set_label(match menu {
            Menu::Wifi => "Wi-Fi",
            Menu::Bluetooth => "Bluetooth",
            Menu::Sound => "Sound Output",
            Menu::Power => "Power Mode",
        });
        while let Some(child) = self.menu_list.first_child() {
            self.menu_list.remove(&child);
        }
        self.menu_list.append(
            &Label::builder()
                .label("Loading…")
                .css_classes(["subtitle"])
                .xalign(0.0)
                .build(),
        );
        self.view.set_visible_child_name("menu");
        self.send(Command::OpenMenu(menu));
        if menu == Menu::Wifi {
            // The scan finishes a few seconds later: list again then.
            let tx = self.commands.clone();
            glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
                let _ = tx.try_send(Command::Refresh);
            });
        }
    }

    fn close_menu(&self) {
        if self.view.visible_child_name().as_deref() != Some("menu") {
            return;
        }
        self.view.set_visible_child_name("main");
        self.password_row.set_visible(false);
        self.stop_typing(true);
        self.send(Command::CloseMenu);
    }

    fn stop_typing(&self, restore_focus: bool) {
        (self.on_typing)(Typing::Ended { restore_focus });
    }

    pub fn update(&self, s: &State) {
        self.state.replace(s.clone());

        self.volume.row.set_visible(s.volume.is_some());
        if let Some((level, muted)) = s.volume {
            self.volume.icon.set_icon_name(if muted {
                "atoll-volume-off-symbolic"
            } else {
                "atoll-volume-symbolic"
            });
            self.volume.value.set_label(&if muted {
                "Muted".to_owned()
            } else {
                percent(level)
            });
            if !self.adjusting.get() {
                self.volume.scale.set_value(level);
            }
        }
        self.brightness.row.set_visible(s.brightness.is_some());
        if let Some(level) = s.brightness {
            self.brightness.value.set_label(&percent(level));
            if !self.adjusting.get() {
                self.brightness.scale.set_value(level);
            }
        }

        set_tile(
            &self.wifi,
            s.wifi.as_ref().map(|(on, net)| {
                let sub = match (on, net) {
                    (false, _) => "Off".to_owned(),
                    (true, Some(n)) => n.clone(),
                    (true, None) => "Not connected".to_owned(),
                };
                (*on, sub)
            }),
        );
        self.wifi
            .icon
            .set_icon_name(Some(if s.wifi.as_ref().is_some_and(|(on, _)| *on) {
                "atoll-wifi-symbolic"
            } else {
                "atoll-wifi-off-symbolic"
            }));
        set_tile(
            &self.bluetooth,
            s.bluetooth.as_ref().map(|(on, dev)| {
                let sub = match (on, dev) {
                    (false, _) => "Off".to_owned(),
                    (true, Some(d)) => d.clone(),
                    (true, None) => "On".to_owned(),
                };
                (*on, sub)
            }),
        );
        self.bluetooth.icon.set_icon_name(Some(
            if s.bluetooth.as_ref().is_some_and(|(on, _)| *on) {
                "atoll-bluetooth-symbolic"
            } else {
                "atoll-bluetooth-off-symbolic"
            },
        ));
        set_tile(
            &self.airplane,
            s.airplane
                .map(|on| (on, if on { "On" } else { "Off" }.to_owned())),
        );
        set_tile(
            &self.power,
            s.profile.as_deref().map(|p| {
                let label = PROFILES
                    .iter()
                    .find(|(id, _)| *id == p)
                    .map_or(p, |(_, label)| label);
                // "Lit" when it isn't the default balanced mode.
                (p != "balanced", label.to_owned())
            }),
        );
        self.lock.root.set_visible(s.can_lock);

        if let Some((menu, items)) = &s.menu {
            self.fill_menu(*menu, items);
        }
        let asking = s.needs_password.is_some();
        if asking && !self.password_row.is_visible() {
            self.password_row.set_visible(true);
            self.password.set_placeholder_text(Some(&format!(
                "Password for {}",
                s.needs_password.as_deref().unwrap_or("")
            )));
            (self.on_typing)(Typing::Started);
            self.password.grab_focus();
        }
    }

    fn fill_menu(&self, menu: Menu, items: &[MenuItem]) {
        // The service answers a pick only once it's done, so a new list
        // means the connection attempt is over.
        self.busy.set(false);
        while let Some(child) = self.menu_list.first_child() {
            self.menu_list.remove(&child);
        }
        if items.is_empty() {
            self.menu_list.append(
                &Label::builder()
                    .label(match menu {
                        Menu::Wifi => "No networks found",
                        Menu::Bluetooth => "No paired devices",
                        _ => "Nothing here",
                    })
                    .css_classes(["subtitle"])
                    .xalign(0.0)
                    .build(),
            );
            return;
        }
        for item in items {
            let text = gtk::Box::builder()
                .orientation(Orientation::Vertical)
                .hexpand(true)
                .build();
            text.append(
                &Label::builder()
                    .label(&item.label)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["cal-event-title"])
                    .build(),
            );
            let detail = Label::builder()
                .label(&item.detail)
                .xalign(0.0)
                .css_classes(["qs-subtitle"])
                .visible(!item.detail.is_empty())
                .build();
            text.append(&detail);
            let content = gtk::Box::builder().spacing(8).build();
            content.append(&text);
            let spinner = gtk::Spinner::builder()
                .visible(false)
                .css_classes(["qs-spinner"])
                .build();
            content.append(&spinner);
            if item.active {
                let check = gtk::Image::from_icon_name("atoll-check-symbolic");
                check.add_css_class("qs-check");
                content.append(&check);
            }
            let row = gtk::Button::builder()
                .child(&content)
                .css_classes(if item.active {
                    ["qs-item", "active"]
                } else {
                    ["qs-item", "idle"]
                })
                .focus_on_click(false)
                .can_focus(false)
                .build();
            row.set_cursor_from_name(Some("pointer"));
            let (tx, id, busy) = (self.commands.clone(), item.id.clone(), self.busy.clone());
            let pending = pending_text(menu, item.active);
            row.connect_clicked(move |row| {
                if busy.replace(true) {
                    return;
                }
                if let Some(text) = pending {
                    detail.set_label(text);
                    detail.set_visible(true);
                    spinner.set_visible(true);
                    spinner.start();
                    row.add_css_class("busy");
                }
                let _ = tx.try_send(Command::Pick(menu, id.clone()));
            });
            self.menu_list.append(&row);
        }
    }
}

/// What a row says while its pick is in progress; quick picks (sound
/// output, power mode) show nothing.
fn pending_text(menu: Menu, active: bool) -> Option<&'static str> {
    match (menu, active) {
        (Menu::Bluetooth | Menu::Wifi, false) => Some("Connecting…"),
        (Menu::Bluetooth, true) => Some("Disconnecting…"),
        _ => None,
    }
}

fn percent(level: f64) -> String {
    format!("{:.0}%", (level * 100.0).clamp(0.0, 150.0))
}

/// Shows the tile with (on, subtitle), or hides it when the control is
/// unavailable.
fn set_tile(t: &Tile, state: Option<(bool, String)>) {
    let Some((on, sub)) = state else {
        t.root.set_visible(false);
        return;
    };
    t.root.set_visible(true);
    t.subtitle.set_label(&sub);
    if on {
        t.root.add_css_class("on");
    } else {
        t.root.remove_css_class("on");
    }
}

/// A tile: the main part toggles; `menu` adds a › that opens its list.
fn tile(icon: &str, title: &str, menu: bool) -> Tile {
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("qs-icon");
    let text = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .valign(Align::Center)
        .build();
    text.append(
        &Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["qs-title"])
            .build(),
    );
    let subtitle = Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(9)
        .css_classes(["qs-subtitle"])
        .build();
    text.append(&subtitle);
    let content = gtk::Box::builder().spacing(8).build();
    content.append(&image);
    content.append(&text);
    let toggle = control_button(&["qs-toggle"], None);
    toggle.set_child(Some(&content));
    toggle.set_halign(Align::Fill);
    toggle.set_hexpand(true);
    let root = gtk::Box::builder().css_classes(["qs-tile"]).build();
    root.append(&toggle);
    if menu {
        let more = control_button(&["qs-more"], Some("atoll-right-symbolic"));
        more.set_tooltip_text(Some("More"));
        root.append(&more);
    }
    Tile {
        root,
        icon: image,
        subtitle,
    }
}

/// A slider row; `menu` adds a › (sound outputs) at the end.
fn slider(icon: &str, tip: &str, menu: bool) -> Slider {
    let button = control_button(&["qs-slider-icon"], Some(icon));
    button.set_tooltip_text(Some(tip));
    let scale = gtk::Scale::builder()
        .orientation(Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(0.0, 0.0, 1.0, 0.02, 0.1, 0.0))
        .draw_value(false)
        .hexpand(true)
        .focus_on_click(false)
        .can_focus(false)
        .build();
    scale.set_cursor_from_name(Some("pointer"));
    let value = Label::builder()
        .css_classes(["time", "qs-value"])
        .xalign(1.0)
        .build();
    let row = gtk::Box::builder().spacing(6).build();
    row.append(&button);
    row.append(&scale);
    row.append(&value);
    if menu {
        let more = control_button(&["qs-more", "plain"], Some("atoll-right-symbolic"));
        more.set_tooltip_text(Some("Sound output"));
        row.append(&more);
    }
    Slider {
        row,
        icon: button,
        scale,
        value,
    }
}
