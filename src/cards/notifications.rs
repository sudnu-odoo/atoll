//! Notifications card: recent notifications, newest first. Clicking one
//! opens it through the Atoll Bridge extension when installed (exactly like
//! GNOME's banner); otherwise it runs a GTK app's own action, or brings the
//! app forward. Clear empties the list.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib, Align, Label, Orientation};

use super::control_button;
use crate::services::notifications::Notification;

/// Taller than the other cards' bodies: chat messages need room to read.
const LIST_H: i32 = 186;
/// Empty, the card is as short as the others (see `island::pages`).
const EMPTY_H: i32 = 132;

pub struct NotificationsCard {
    pub root: gtk::Box,
    list: gtk::Box,
    empty: gtk::Box,
    scroller: gtk::ScrolledWindow,
    clear: gtk::Button,
    /// `None` clears all, `Some(id)` removes one.
    on_clear: Rc<dyn Fn(Option<u64>)>,
}

impl NotificationsCard {
    pub fn new(on_clear: impl Fn(Option<u64>) + 'static) -> Rc<Self> {
        let on_clear: Rc<dyn Fn(Option<u64>)> = Rc::new(on_clear);
        // The island's shared top row (tabs, clock, settings) names the card
        // and shows the time; this row keeps only the card's own actions.
        // Clear sits over the GNOME bar; its clicks arrive through the
        // island's bar-click handling.
        let clear = control_button(&["shelf-clear"], None);
        clear.set_label("Clear");
        let clear_all = on_clear.clone();
        clear.connect_clicked(move |_| clear_all(None));
        let top = gtk::CenterBox::builder()
            .css_classes(["card-top"])
            .start_widget(&clear)
            .build();

        let list = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(6)
            .build();
        let empty = super::empty_state("atoll-bell-off-symbolic", "No notifications");
        // Long lists scroll; scrolling here doesn't flip cards.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(LIST_H)
            .max_content_height(LIST_H)
            .child(&list)
            .build();

        let root = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["water-card"])
            .width_request(crate::island::EXPANDED_NOTIFICATIONS.0 as i32)
            .height_request(crate::island::EXPANDED_NOTIFICATIONS.1 as i32)
            .valign(Align::Start)
            .build();
        root.append(&top);
        let body = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .css_classes(["todo-body"])
            .build();
        body.append(&scroller);
        root.append(&body);

        let card = Rc::new(Self {
            root,
            list,
            empty,
            scroller: scroller.clone(),
            clear,
            on_clear,
        });
        card.update(&[]);
        card
    }

    /// `items` newest first.
    pub fn update(&self, items: &[Notification]) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.clear.set_visible(!items.is_empty());
        let (h, card_h) = if items.is_empty() {
            (EMPTY_H, crate::island::EXPANDED_NOTIFICATIONS_EMPTY.1)
        } else {
            (LIST_H, crate::island::EXPANDED_NOTIFICATIONS.1)
        };
        self.scroller.set_min_content_height(h);
        self.scroller.set_max_content_height(h);
        self.root.set_height_request(card_h as i32);
        if items.is_empty() {
            self.list.append(&self.empty);
            return;
        }
        let now = glib::DateTime::now_local().map_or(0, |t| t.to_unix());
        for n in items {
            self.list.append(&row(n, now, self.on_clear.clone()));
        }
    }
}

fn row(n: &Notification, now: i64, on_clear: Rc<dyn Fn(Option<u64>)>) -> gtk::Button {
    let time = Label::builder()
        .label(ago(now - n.at))
        .css_classes(["qs-subtitle"])
        .valign(Align::Center)
        .build();
    let title = Label::builder()
        // One line: a title with line breaks would stack out of its row.
        .label(n.title.split_whitespace().collect::<Vec<_>>().join(" "))
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["notif-title"])
        .build();
    super::full_text_on_hover(&title);
    let text = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .valign(Align::Center)
        .build();
    if n.app.is_empty() {
        // No app name (common for web apps, Discord): the time joins the
        // title line instead of holding up an empty line above it.
        let line = gtk::Box::builder().spacing(8).build();
        line.append(&title);
        line.append(&time);
        text.append(&line);
    } else {
        let head = gtk::Box::builder().spacing(8).build();
        head.append(
            &Label::builder()
                .label(n.app.to_uppercase())
                .css_classes(["caps", "muted"])
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
        );
        head.append(&time);
        text.append(&head);
        if !n.title.is_empty() {
            text.append(&title);
        }
    }
    if !n.body.is_empty() {
        let body = Label::builder()
            .label(n.body.replace('\n', " "))
            .xalign(0.0)
            .wrap(true)
            // Long links have no spaces to break at.
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(3)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            // Wrap to the row's real width: a character-count cap could
            // be wider than the row and spill past its background.
            .max_width_chars(1)
            .hexpand(true)
            .css_classes(["notif-body"])
            .build();
        super::full_text_on_hover(&body);
        text.append(&body);
    }
    text.set_hexpand(true);
    let content = gtk::Box::builder().spacing(12).build();
    content.append(&picture(n, 36));
    content.append(&text);
    let button = gtk::Button::builder()
        .child(&content)
        .css_classes(["qs-item", "idle", "notif-row"])
        .focus_on_click(false)
        .can_focus(false)
        .build();
    button.set_cursor_from_name(Some("pointer"));
    let (click, app, n) = (n.click.clone(), app_for(n), n.clone());
    button.connect_clicked(move |_| {
        use crate::services::notifications::{activate_app_action, activate_via_shell, Click};
        // Opened means seen: it leaves the list, like GNOME's own.
        on_clear(Some(n.id));
        let (click, app) = (click.clone(), app.clone());
        let answer = activate_via_shell(&n);
        glib::spawn_future_local(async move {
            if answer.recv().await == Ok(true) {
                return;
            }
            match &click {
                Click::AppAction {
                    app_id,
                    action,
                    target,
                } => activate_app_action(app_id.clone(), action.clone(), target.clone()),
                Click::OpenApp => {
                    if let Some(app) = &app {
                        if let Err(e) = app.launch(&[], None::<&gio::AppLaunchContext>) {
                            tracing::warn!("cannot open app: {e}");
                        }
                    }
                }
            }
        });
    });
    button
}

fn ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
    }
}

/// The picture for a notification: the image it carried (avatar, cover),
/// else its icon, else the sending app's own icon.
pub fn icon_for(n: &Notification) -> Option<gio::Icon> {
    use crate::services::notifications::Icon;
    match &n.icon {
        Some(Icon::Pixels {
            width,
            height,
            stride,
            alpha,
            bytes,
        }) => {
            let format = if *alpha {
                gtk::gdk::MemoryFormat::R8g8b8a8
            } else {
                gtk::gdk::MemoryFormat::R8g8b8
            };
            let texture = gtk::gdk::MemoryTexture::new(
                *width,
                *height,
                format,
                &glib::Bytes::from(bytes),
                usize::try_from(*stride).ok()?,
            );
            Some(texture.upcast())
        }
        Some(Icon::File(path)) if path.exists() => {
            Some(gio::FileIcon::new(&gio::File::for_path(path)).upcast())
        }
        Some(Icon::Named(name)) => Some(gio::ThemedIcon::new(name).upcast()),
        _ => app_icon(n),
    }
}

/// The sending app: by its desktop entry, else its name, else the app whose
/// icon the notification named (Discord often gives only that). Launching it
/// again brings its window forward.
fn app_for(n: &Notification) -> Option<gio::AppInfo> {
    use crate::services::notifications::Icon;
    let apps = gio::AppInfo::all();
    let lower = |s: &str| s.to_lowercase();
    if let Some(id) = n.desktop_entry.as_deref() {
        let id = if id.ends_with(".desktop") {
            id.to_owned()
        } else {
            format!("{id}.desktop")
        };
        if let Some(a) = apps.iter().find(|a| a.id().as_deref() == Some(id.as_str())) {
            return Some(a.clone());
        }
    }
    let name = lower(&n.app);
    if !name.is_empty() {
        let hit = apps.iter().find(|a| {
            lower(&a.display_name()).contains(&name)
                || a.id().is_some_and(|id| lower(&id).contains(&name))
        });
        if let Some(a) = hit {
            return Some(a.clone());
        }
    }
    if let Some(Icon::Named(icon)) = &n.icon {
        let icon = lower(icon);
        return apps
            .iter()
            .find(|a| {
                a.icon()
                    .and_then(|i| i.to_string())
                    .is_some_and(|s| lower(&s).contains(&icon))
            })
            .cloned();
    }
    None
}

/// The app's icon, for notifications that carry no picture of their own.
fn app_icon(n: &Notification) -> Option<gio::Icon> {
    app_for(n)?.icon()
}

/// The notification's picture, `size` px square: images (avatars, covers)
/// are cropped to fill like the music cover; icons keep their shape. A bell
/// when nothing is known.
pub fn picture(n: &Notification, size: i32) -> gtk::Widget {
    use crate::services::notifications::Icon;
    let texture = match (&n.icon, icon_for(n)) {
        (Some(Icon::Pixels { .. }), Some(icon)) => icon.downcast::<gtk::gdk::Texture>().ok(),
        (Some(Icon::File(path)), _) => gtk::gdk::Texture::from_filename(path).ok(),
        _ => None,
    };
    if let Some(texture) = texture {
        let cover = super::widgets::Cover::new(size, 10.0);
        cover.set(Some(&texture));
        cover.area.set_valign(Align::Center);
        return cover.area.upcast();
    }
    let image = match icon_for(n) {
        Some(icon) => gtk::Image::from_gicon(&icon),
        None => gtk::Image::from_icon_name("atoll-bell-symbolic"),
    };
    image.set_pixel_size(size);
    image.set_valign(Align::Center);
    image.upcast()
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn relative_ages() {
        assert_eq!(ago(-5), "now");
        assert_eq!(ago(30), "now");
        assert_eq!(ago(125), "2 min");
        assert_eq!(ago(7200), "2 h");
        assert_eq!(ago(200_000), "2 d");
    }
}
