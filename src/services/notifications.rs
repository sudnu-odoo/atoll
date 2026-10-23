//! Notifications, observed as apps send them. GNOME Shell owns the
//! notification service (only an extension could replace it), so Atoll
//! becomes a session-bus *monitor* for just the two notification calls,
//! freedesktop `Notify` and GTK's `AddNotification`, and mirrors them.
//!
//! A monitor connection can only listen, never reply, and the match rules
//! keep it from seeing any other traffic on the bus.

use std::collections::HashMap;

use futures_util::StreamExt;
use zbus::fdo::MonitoringProxy;
use zbus::message::Type;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, MatchRule, MessageStream};

#[derive(Clone, Debug)]
pub struct Notification {
    /// Human app name ("Brave", "Discord"), possibly empty.
    pub app: String,
    /// Desktop file id ("brave-browser"), to bring the app forward.
    pub desktop_entry: Option<String>,
    pub title: String,
    pub body: String,
    /// Unix seconds when it arrived.
    pub at: i64,
    pub icon: Option<Icon>,
    /// What clicking it should do.
    pub click: Click,
}

/// How a click can reach the notification's own target.
#[derive(Clone, Debug)]
pub enum Click {
    /// A GTK/GApplication notification: its default action, which the app
    /// itself can be asked to run (no GNOME Shell needed).
    AppAction {
        app_id: String,
        /// Without the "app." prefix.
        action: String,
        target: Option<std::sync::Arc<OwnedValue>>,
    },
    /// Anything else (Discord, browsers): only GNOME Shell can deliver
    /// their own click, so the app is brought forward instead.
    OpenApp,
}

/// Asks a GApplication to run one of its actions, over the standard
/// org.freedesktop.Application interface; the same thing GNOME Shell does
/// when its notification is clicked.
pub fn activate_app_action(
    app_id: String,
    action: String,
    target: Option<std::sync::Arc<OwnedValue>>,
) {
    let _ = std::thread::Builder::new()
        .name("notif-action".into())
        .spawn(move || {
            let result = (|| -> zbus::Result<()> {
                let conn = zbus::blocking::Connection::session()?;
                let path = format!("/{}", app_id.replace('.', "/").replace('-', "_"));
                let params: Vec<zbus::zvariant::Value> = target
                    .iter()
                    .filter_map(|t| t.try_clone().ok())
                    .map(Into::into)
                    .collect();
                let platform: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
                conn.call_method(
                    Some(app_id.as_str()),
                    path.as_str(),
                    Some("org.freedesktop.Application"),
                    "ActivateAction",
                    &(action.as_str(), params, platform),
                )?;
                Ok(())
            })();
            if let Err(e) = result {
                tracing::warn!("cannot run {app_id} action {action}: {e}");
            }
        });
}

/// Where a notification's picture comes from, as plain data (the UI turns
/// it into an icon; GTK objects can't come from this thread).
#[derive(Clone, Debug)]
pub enum Icon {
    /// The image sent with the notification, e.g. a sender's avatar.
    Pixels {
        width: i32,
        height: i32,
        stride: i32,
        alpha: bool,
        bytes: Vec<u8>,
    },
    File(std::path::PathBuf),
    /// A themed icon name.
    Named(String),
}

pub fn spawn(updates: async_channel::Sender<Notification>) {
    let spawned = std::thread::Builder::new()
        .name("notifications".into())
        .spawn(move || {
            if let Err(e) = async_io::block_on(run(&updates)) {
                tracing::warn!("notifications unavailable: {e}");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start notifications thread: {e}");
    }
}

async fn run(updates: &async_channel::Sender<Notification>) -> zbus::Result<()> {
    // A dedicated connection: once it becomes a monitor it can't be used
    // for anything else.
    let conn = Connection::session().await?;
    let rules = [
        MatchRule::builder()
            .msg_type(Type::MethodCall)
            .interface("org.freedesktop.Notifications")?
            .member("Notify")?
            .build(),
        MatchRule::builder()
            .msg_type(Type::MethodCall)
            .interface("org.gtk.Notifications")?
            .member("AddNotification")?
            .build(),
    ];
    MonitoringProxy::new(&conn)
        .await?
        .become_monitor(&rules, 0)
        .await?;
    tracing::debug!("watching notifications");

    let mut stream = MessageStream::from(&conn);
    // Some notifications reach the monitor twice (e.g. once per hop);
    // an identical one within a moment is the same notification.
    let mut last: Option<(String, String, String, i64)> = None;
    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };
        let header = msg.header();
        let parsed = match header.member().map(|m| m.as_str()) {
            Some("Notify") => freedesktop(&msg),
            Some("AddNotification") => gtk(&msg),
            _ => None,
        };
        let Some(n) = parsed else { continue };
        let key = (n.app.clone(), n.title.clone(), n.body.clone(), n.at);
        if last
            .as_ref()
            .is_some_and(|(a, t, b, at)| (a, t, b) == (&key.0, &key.1, &key.2) && key.3 - at <= 2)
        {
            continue;
        }
        last = Some(key);
        if updates.send(n).await.is_err() {
            break;
        }
    }
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `Notify(app_name, replaces_id, app_icon, summary, body, actions, hints,
/// expire_timeout)`.
fn freedesktop(msg: &zbus::Message) -> Option<Notification> {
    type Args = (
        String,
        u32,
        String,
        String,
        String,
        Vec<String>,
        HashMap<String, OwnedValue>,
        i32,
    );
    let (app, _, app_icon, summary, body, _, hints, _): Args = msg.body().deserialize().ok()?;
    let desktop_entry = hints
        .get("desktop-entry")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| String::try_from(v).ok());
    // The spec's image order: image-data, then image-path, then app_icon.
    type Raw = (i32, i32, i32, bool, i32, i32, Vec<u8>);
    let pixels = ["image-data", "image_data", "icon_data"]
        .iter()
        .find_map(|k| hints.get(*k)?.try_clone().ok())
        .and_then(|v| Raw::try_from(v).ok())
        .map(|(width, height, stride, alpha, _, _, bytes)| Icon::Pixels {
            width,
            height,
            stride,
            alpha,
            bytes,
        });
    let path = ["image-path", "image_path"]
        .iter()
        .find_map(|k| hints.get(*k)?.try_clone().ok())
        .and_then(|v| String::try_from(v).ok());
    let icon = pixels
        .or_else(|| path.as_deref().and_then(icon_from_text))
        .or_else(|| icon_from_text(&app_icon));
    Some(Notification {
        app,
        desktop_entry,
        title: summary,
        body: strip_markup(&body),
        at: now(),
        icon,
        click: Click::OpenApp,
    })
}

/// `AddNotification(app_id, id, notification)` from GApplication apps.
fn gtk(msg: &zbus::Message) -> Option<Notification> {
    let (app_id, _, n): (String, String, HashMap<String, OwnedValue>) =
        msg.body().deserialize().ok()?;
    let text = |k: &str| {
        n.get(k)
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| String::try_from(v).ok())
            .unwrap_or_default()
    };
    let app = app_id.rsplit('.').next().unwrap_or(&app_id).to_owned();
    let click = text("default-action")
        .strip_prefix("app.")
        .map(|action| Click::AppAction {
            app_id: app_id.clone(),
            action: action.to_owned(),
            target: n
                .get("default-action-target")
                .and_then(|v| v.try_clone().ok())
                .map(std::sync::Arc::new),
        })
        .unwrap_or(Click::OpenApp);
    Some(Notification {
        app,
        desktop_entry: Some(app_id),
        title: text("title"),
        body: strip_markup(&text("body")),
        at: now(),
        // The UI falls back to the app's own icon via the desktop entry.
        icon: None,
        click,
    })
}

/// "/path/pic.png", "file:///…" or an icon name.
fn icon_from_text(s: &str) -> Option<Icon> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(p) = s.strip_prefix("file://") {
        return Some(Icon::File(p.into()));
    }
    if s.starts_with('/') {
        return Some(Icon::File(s.into()));
    }
    Some(Icon::Named(s.to_owned()))
}

/// Notification bodies may carry simple markup (<b>, <a href>); show text.
fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}
