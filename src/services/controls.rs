//! Quick settings: Wi-Fi, Bluetooth, airplane mode, power mode, volume,
//! brightness, lock. Reads happen when the card asks (it's shown, or after a
//! change), so nothing runs while the card is closed. Every control is
//! optional: if its service is missing, its state is `None` and it hides.
//!
//! All through services a normal user may use (no root, no gsettings):
//! NetworkManager, GNOME's rfkill service, power-profiles-daemon, PipeWire
//! (`wpctl`), logind (backlight) and GNOME's screensaver.

use std::process::Command as Process;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

#[derive(Clone, Debug, Default)]
pub struct State {
    /// (enabled, connected network name)
    pub wifi: Option<(bool, Option<String>)>,
    /// (powered, connected device name)
    pub bluetooth: Option<(bool, Option<String>)>,
    pub airplane: Option<bool>,
    /// "power-saver", "balanced" or "performance"
    pub profile: Option<String>,
    /// (0.0–1.0, muted)
    pub volume: Option<(f64, bool)>,
    /// 0.0–1.0 of the laptop panel
    pub brightness: Option<f64>,
    pub can_lock: bool,
    /// Items of the menu the card has open (Wi-Fi networks, devices...).
    pub menu: Option<(Menu, Vec<MenuItem>)>,
    /// A secured network with no saved password: the card asks for one.
    pub needs_password: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Menu {
    Wifi,
    Bluetooth,
    Sound,
    Power,
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    /// What `Pick` sends back: SSID, device path, sink id or profile.
    pub id: String,
    pub label: String,
    pub detail: String,
    pub active: bool,
}

pub enum Command {
    Refresh,
    SetWifi(bool),
    SetBluetooth(bool),
    SetAirplane(bool),
    SetProfile(String),
    SetVolume(f64),
    ToggleMute,
    SetBrightness(f64),
    Lock,
    /// List the items of a menu (and keep listing it after changes).
    OpenMenu(Menu),
    CloseMenu,
    Pick(Menu, String),
    /// Join a secured Wi-Fi network with this password.
    JoinWifi(String, String),
}

pub fn spawn(updates: async_channel::Sender<State>) -> async_channel::Sender<Command> {
    let (tx, rx) = async_channel::unbounded::<Command>();
    let spawned = std::thread::Builder::new()
        .name("controls".into())
        .spawn(move || {
            async_io::block_on(async move {
                let system = Connection::system().await.ok();
                let session = Connection::session().await.ok();
                // The menu the card has open, re-listed after every change.
                let mut menu: Option<Menu> = None;
                let mut needs_password: Option<String> = None;
                while let Ok(cmd) = rx.recv().await {
                    // A burst of slider moves: only the latest matters.
                    let mut cmd = cmd;
                    while let Ok(next) = rx.try_recv() {
                        let both_volume = matches!(
                            (&cmd, &next),
                            (Command::SetVolume(_), Command::SetVolume(_))
                        );
                        let both_light = matches!(
                            (&cmd, &next),
                            (Command::SetBrightness(_), Command::SetBrightness(_))
                        );
                        if both_volume || both_light {
                            cmd = next;
                        } else {
                            apply(cmd, system.as_ref(), session.as_ref()).await;
                            cmd = next;
                        }
                    }
                    match cmd {
                        Command::OpenMenu(m) => {
                            menu = Some(m);
                            needs_password = None;
                            if m == Menu::Wifi {
                                request_wifi_scan(system.as_ref()).await;
                            }
                        }
                        Command::CloseMenu => {
                            menu = None;
                            needs_password = None;
                        }
                        Command::Pick(m, id) => {
                            needs_password = pick(m, &id, system.as_ref(), session.as_ref()).await;
                        }
                        Command::JoinWifi(ssid, password) => {
                            needs_password = None;
                            join_wifi(system.as_ref(), &ssid, Some(&password)).await;
                        }
                        other => apply(other, system.as_ref(), session.as_ref()).await,
                    }
                    let mut state = read(system.as_ref(), session.as_ref()).await;
                    if let Some(m) = menu {
                        state.menu = Some((m, list(m, system.as_ref(), &state).await));
                    }
                    state.needs_password = needs_password.clone();
                    if updates.send(state).await.is_err() {
                        return;
                    }
                }
            });
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start controls thread: {e}");
    }
    tx
}

const NM: (&str, &str, &str) = (
    "org.freedesktop.NetworkManager",
    "/org/freedesktop/NetworkManager",
    "org.freedesktop.NetworkManager",
);
const RFKILL: (&str, &str, &str) = (
    "org.gnome.SettingsDaemon.Rfkill",
    "/org/gnome/SettingsDaemon/Rfkill",
    "org.gnome.SettingsDaemon.Rfkill",
);
const PROFILES: (&str, &str, &str) = (
    "org.freedesktop.UPower.PowerProfiles",
    "/org/freedesktop/UPower/PowerProfiles",
    "org.freedesktop.UPower.PowerProfiles",
);
const LOGIND_SESSION: (&str, &str, &str) = (
    "org.freedesktop.login1",
    "/org/freedesktop/login1/session/auto",
    "org.freedesktop.login1.Session",
);
const SCREENSAVER: (&str, &str, &str) = (
    "org.gnome.ScreenSaver",
    "/org/gnome/ScreenSaver",
    "org.gnome.ScreenSaver",
);
const SINK: &str = "@DEFAULT_AUDIO_SINK@";

async fn proxy<'a>(
    conn: Option<&Connection>,
    (bus, path, iface): (&'a str, &'a str, &'a str),
) -> Option<Proxy<'a>> {
    Proxy::new(conn?, bus, path, iface).await.ok()
}

async fn get<T: TryFrom<OwnedValue>>(p: &Proxy<'_>, name: &str) -> Option<T> {
    let v: OwnedValue = p.get_property(name).await.ok()?;
    T::try_from(v).ok()
}

async fn set(p: Option<Proxy<'_>>, name: &str, value: Value<'static>) {
    let Some(p) = p else { return };
    if let Err(e) = p.set_property(name, value).await {
        tracing::warn!("cannot set {name}: {e}");
    }
}

async fn apply(cmd: Command, system: Option<&Connection>, session: Option<&Connection>) {
    match cmd {
        Command::Refresh
        | Command::OpenMenu(_)
        | Command::CloseMenu
        | Command::Pick(..)
        | Command::JoinWifi(..) => {}
        Command::SetWifi(on) => {
            set(proxy(system, NM).await, "WirelessEnabled", Value::from(on)).await
        }
        // GNOME's own Bluetooth toggle goes through rfkill, not BlueZ.
        Command::SetBluetooth(on) => {
            set(
                proxy(session, RFKILL).await,
                "BluetoothAirplaneMode",
                Value::from(!on),
            )
            .await
        }
        Command::SetAirplane(on) => {
            set(
                proxy(session, RFKILL).await,
                "AirplaneMode",
                Value::from(on),
            )
            .await
        }
        Command::SetProfile(name) => {
            set(
                proxy(system, PROFILES).await,
                "ActiveProfile",
                Value::from(name),
            )
            .await
        }
        Command::SetVolume(v) => run(
            "wpctl",
            &["set-volume", SINK, &format!("{:.2}", v.clamp(0.0, 1.0))],
        ),
        Command::ToggleMute => run("wpctl", &["set-mute", SINK, "toggle"]),
        Command::SetBrightness(f) => {
            let Some((name, max)) = backlight() else {
                return;
            };
            let value = (f.clamp(0.02, 1.0) * max as f64).round() as u32;
            if let Some(p) = proxy(system, LOGIND_SESSION).await {
                if let Err(e) = p
                    .call_method("SetBrightness", &("backlight", name.as_str(), value))
                    .await
                {
                    tracing::warn!("cannot set brightness: {e}");
                }
            }
        }
        Command::Lock => {
            if let Some(p) = proxy(session, SCREENSAVER).await {
                if let Err(e) = p.call_method("Lock", &()).await {
                    tracing::warn!("cannot lock: {e}");
                }
            }
        }
    }
}

async fn read(system: Option<&Connection>, session: Option<&Connection>) -> State {
    let mut s = State::default();

    if let Some(nm) = proxy(system, NM).await {
        if let Some(enabled) = get::<bool>(&nm, "WirelessEnabled").await {
            s.wifi = Some((enabled, primary_connection_name(system, &nm).await));
        }
    }
    if let Some(rf) = proxy(session, RFKILL).await {
        s.airplane = get::<bool>(&rf, "AirplaneMode").await;
        if get::<bool>(&rf, "BluetoothHasAirplaneMode").await == Some(true) {
            let blocked = get::<bool>(&rf, "BluetoothAirplaneMode")
                .await
                .unwrap_or(false);
            s.bluetooth = Some((!blocked, connected_bluetooth(system).await));
        }
    }
    if let Some(pp) = proxy(system, PROFILES).await {
        s.profile = get::<String>(&pp, "ActiveProfile").await;
    }
    s.volume = volume();
    s.brightness = backlight().and_then(|(name, max)| {
        let now: u64 = std::fs::read_to_string(format!("/sys/class/backlight/{name}/brightness"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        Some(now as f64 / max.max(1) as f64)
    });
    s.can_lock = proxy(session, SCREENSAVER).await.is_some();
    s
}

/// The name NetworkManager shows for the main connection (Wi-Fi SSID or
/// "Wired connection 1").
async fn primary_connection_name(system: Option<&Connection>, nm: &Proxy<'_>) -> Option<String> {
    let path: OwnedObjectPath = get(nm, "PrimaryConnection").await?;
    if path.as_str() == "/" {
        return None;
    }
    let active = Proxy::new(
        system?,
        "org.freedesktop.NetworkManager",
        path,
        "org.freedesktop.NetworkManager.Connection.Active",
    )
    .await
    .ok()?;
    get::<String>(&active, "Id").await
}

async fn connected_bluetooth(system: Option<&Connection>) -> Option<String> {
    let manager = zbus::fdo::ObjectManagerProxy::builder(system?)
        .destination("org.bluez")
        .ok()?
        .path("/")
        .ok()?
        .build()
        .await
        .ok()?;
    let objects = manager.get_managed_objects().await.ok()?;
    objects.values().find_map(|ifaces| {
        let dev = ifaces.get("org.bluez.Device1")?;
        let connected = dev
            .get("Connected")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        connected
            .then(|| dev.get("Alias")?.try_clone().ok())
            .flatten()
            .and_then(|v| String::try_from(v).ok())
    })
}

/// "Volume: 0.48 [MUTED]" from wpctl.
fn volume() -> Option<(f64, bool)> {
    let out = Process::new("wpctl")
        .args(["get-volume", SINK])
        .output()
        .ok()?;
    parse_volume(&String::from_utf8_lossy(&out.stdout))
}

/// "Volume: 0.48" or "Volume: 0.48 [MUTED]".
fn parse_volume(text: &str) -> Option<(f64, bool)> {
    let level: f64 = text.split_whitespace().nth(1)?.parse().ok()?;
    Some((level, text.contains("MUTED")))
}

/// The first kernel backlight and its maximum.
fn backlight() -> Option<(String, u64)> {
    let entry = std::fs::read_dir("/sys/class/backlight")
        .ok()?
        .flatten()
        .next()?;
    let name = entry.file_name().to_string_lossy().into_owned();
    let max = std::fs::read_to_string(entry.path().join("max_brightness"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((name, max))
}

fn run(program: &str, args: &[&str]) {
    match Process::new(program).args(args).status() {
        Ok(s) if s.success() => {}
        Ok(s) => tracing::warn!("{program} {args:?} exited with {s}"),
        Err(e) => tracing::warn!("cannot run {program}: {e}"),
    }
}

// ---- Menus -------------------------------------------------------------

const PROFILE_NAMES: [(&str, &str); 3] = [
    ("power-saver", "Power Saver"),
    ("balanced", "Balanced"),
    ("performance", "Performance"),
];

async fn list(menu: Menu, system: Option<&Connection>, state: &State) -> Vec<MenuItem> {
    match menu {
        Menu::Wifi => wifi_networks(system).await,
        Menu::Bluetooth => bluetooth_devices(system).await,
        Menu::Sound => sinks(),
        Menu::Power => PROFILE_NAMES
            .iter()
            .map(|(id, label)| MenuItem {
                id: (*id).into(),
                label: (*label).into(),
                detail: String::new(),
                active: state.profile.as_deref() == Some(*id),
            })
            .collect(),
    }
}

/// Acts on a picked item. Returns an SSID when that network needs a
/// password first.
async fn pick(
    menu: Menu,
    id: &str,
    system: Option<&Connection>,
    session: Option<&Connection>,
) -> Option<String> {
    match menu {
        Menu::Wifi => {
            let networks = wifi_networks(system).await;
            let net = networks.iter().find(|n| n.id == id)?;
            let secure = net.detail.contains("Secured");
            let known = net.detail.contains("Saved");
            if secure && !known && !net.active {
                return Some(id.to_owned());
            }
            if !net.active {
                join_wifi(system, id, None).await;
            }
            None
        }
        Menu::Bluetooth => {
            let connected = bluetooth_devices(system)
                .await
                .iter()
                .any(|d| d.id == id && d.active);
            let path = OwnedObjectPath::try_from(id.to_owned()).ok()?;
            let dev = Proxy::new(system?, "org.bluez", path, "org.bluez.Device1")
                .await
                .ok()?;
            let method = if connected { "Disconnect" } else { "Connect" };
            if let Err(e) = dev.call_method(method, &()).await {
                tracing::warn!("bluetooth {method} failed: {e}");
            }
            None
        }
        Menu::Sound => {
            run("wpctl", &["set-default", id]);
            None
        }
        Menu::Power => {
            apply(Command::SetProfile(id.to_owned()), system, session).await;
            None
        }
    }
}

/// The wireless device's D-Bus path.
async fn wifi_device(system: Option<&Connection>) -> Option<OwnedObjectPath> {
    let nm = proxy(system, NM).await?;
    let devices: Vec<OwnedObjectPath> = nm.call("GetDevices", &()).await.ok()?;
    for path in devices {
        let dev = Proxy::new(
            system?,
            "org.freedesktop.NetworkManager",
            path.clone(),
            "org.freedesktop.NetworkManager.Device",
        )
        .await
        .ok()?;
        // NM_DEVICE_TYPE_WIFI
        if get::<u32>(&dev, "DeviceType").await == Some(2) {
            return Some(path);
        }
    }
    None
}

async fn wireless(system: Option<&Connection>) -> Option<(OwnedObjectPath, Proxy<'static>)> {
    let path = wifi_device(system).await?;
    let p = Proxy::new(
        system?,
        "org.freedesktop.NetworkManager",
        path.clone(),
        "org.freedesktop.NetworkManager.Device.Wireless",
    )
    .await
    .ok()?;
    Some((path, p))
}

async fn request_wifi_scan(system: Option<&Connection>) {
    let Some((_, w)) = wireless(system).await else {
        return;
    };
    // Fresh results arrive a few seconds later; the card re-lists then.
    let opts: std::collections::HashMap<&str, Value> = Default::default();
    let _ = w.call_method("RequestScan", &(opts,)).await;
}

type ConnSettings =
    std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>>;

/// SSIDs that have a saved connection, with that connection's path.
async fn saved_networks(system: Option<&Connection>) -> Vec<(String, OwnedObjectPath)> {
    let Some(conn) = system else {
        return Vec::new();
    };
    let Ok(settings) = Proxy::new(
        conn,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager/Settings",
        "org.freedesktop.NetworkManager.Settings",
    )
    .await
    else {
        return Vec::new();
    };
    let Ok(paths) = settings
        .call::<_, _, Vec<OwnedObjectPath>>("ListConnections", &())
        .await
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for path in paths {
        let Ok(c) = Proxy::new(
            conn,
            "org.freedesktop.NetworkManager",
            path.clone(),
            "org.freedesktop.NetworkManager.Settings.Connection",
        )
        .await
        else {
            continue;
        };
        let Ok(s) = c.call::<_, _, ConnSettings>("GetSettings", &()).await else {
            continue;
        };
        let ssid = s
            .get("802-11-wireless")
            .and_then(|w| w.get("ssid"))
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<u8>::try_from(v).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        if let Some(ssid) = ssid {
            out.push((ssid, path));
        }
    }
    out
}

async fn wifi_networks(system: Option<&Connection>) -> Vec<MenuItem> {
    let (Some(conn), Some((_, w))) = (system, wireless(system).await) else {
        return Vec::new();
    };
    let active: Option<OwnedObjectPath> = get(&w, "ActiveAccessPoint").await;
    let aps: Vec<OwnedObjectPath> = get(&w, "AccessPoints").await.unwrap_or_default();
    let saved: Vec<String> = saved_networks(system)
        .await
        .into_iter()
        .map(|(s, _)| s)
        .collect();

    // Strongest access point per network name: (strength, secure, active).
    let mut best: std::collections::HashMap<String, (u8, bool, bool)> = Default::default();
    for ap_path in aps {
        let is_active = active.as_ref() == Some(&ap_path);
        let Ok(ap) = Proxy::new(
            conn,
            "org.freedesktop.NetworkManager",
            ap_path,
            "org.freedesktop.NetworkManager.AccessPoint",
        )
        .await
        else {
            continue;
        };
        let ssid = get::<Vec<u8>>(&ap, "Ssid")
            .await
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        if ssid.is_empty() {
            continue; // hidden network
        }
        let strength = get::<u8>(&ap, "Strength").await.unwrap_or(0);
        let secure = get::<u32>(&ap, "WpaFlags").await.unwrap_or(0) != 0
            || get::<u32>(&ap, "RsnFlags").await.unwrap_or(0) != 0
            || get::<u32>(&ap, "Flags").await.unwrap_or(0) & 1 != 0;
        let entry = best.entry(ssid).or_insert((0, secure, false));
        entry.0 = entry.0.max(strength);
        entry.2 |= is_active;
    }
    let mut items: Vec<(u8, MenuItem)> = best
        .into_iter()
        .map(|(ssid, (strength, secure, active))| {
            let mut detail = vec![format!("{strength}%")];
            if active {
                detail.insert(0, "Connected".into());
            }
            if saved.contains(&ssid) {
                detail.push("Saved".into());
            }
            if secure {
                detail.push("Secured".into());
            }
            let item = MenuItem {
                id: ssid.clone(),
                label: ssid,
                detail: detail.join(" · "),
                active,
            };
            (strength, item)
        })
        .collect();
    // Connected first, then by signal.
    items.sort_by(|a, b| b.1.active.cmp(&a.1.active).then(b.0.cmp(&a.0)));
    items.into_iter().map(|(_, i)| i).take(12).collect()
}

/// Connects to a network: its saved connection if there is one, otherwise a
/// new one (with `password` for secured networks).
async fn join_wifi(system: Option<&Connection>, ssid: &str, password: Option<&str>) {
    let (Some(device), Some(nm)) = (wifi_device(system).await, proxy(system, NM).await) else {
        return;
    };
    let Ok(root) = OwnedObjectPath::try_from("/") else {
        return;
    };
    let saved = saved_networks(system)
        .await
        .into_iter()
        .find(|(s, _)| s == ssid);
    let result = match saved {
        Some((_, path)) if password.is_none() => nm
            .call_method("ActivateConnection", &(path, device, root))
            .await
            .map(|_| ()),
        _ => {
            use std::collections::HashMap;
            let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
            settings.insert(
                "802-11-wireless",
                HashMap::from([("ssid", Value::from(ssid.as_bytes().to_vec()))]),
            );
            if let Some(psk) = password {
                settings.insert(
                    "802-11-wireless-security",
                    HashMap::from([
                        ("key-mgmt", Value::from("wpa-psk")),
                        ("psk", Value::from(psk.to_owned())),
                    ]),
                );
            }
            nm.call_method("AddAndActivateConnection", &(settings, device, root))
                .await
                .map(|_| ())
        }
    };
    if let Err(e) = result {
        tracing::warn!("cannot join {ssid}: {e}");
    }
}

async fn bluetooth_devices(system: Option<&Connection>) -> Vec<MenuItem> {
    let Some(conn) = system else {
        return Vec::new();
    };
    let Ok(builder) = zbus::fdo::ObjectManagerProxy::builder(conn)
        .destination("org.bluez")
        .and_then(|b| b.path("/"))
    else {
        return Vec::new();
    };
    let Ok(manager) = builder.build().await else {
        return Vec::new();
    };
    let Ok(objects) = manager.get_managed_objects().await else {
        return Vec::new();
    };
    let flag = |d: &std::collections::HashMap<String, OwnedValue>, k: &str| {
        d.get(k)
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false)
    };
    let mut items: Vec<MenuItem> = objects
        .iter()
        .filter_map(|(path, ifaces)| {
            let dev = ifaces.get("org.bluez.Device1")?;
            if !flag(dev, "Paired") {
                return None;
            }
            let name = dev
                .get("Alias")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| String::try_from(v).ok())?;
            let connected = flag(dev, "Connected");
            Some(MenuItem {
                id: path.to_string(),
                label: name,
                detail: if connected {
                    "Connected"
                } else {
                    "Not connected"
                }
                .into(),
                active: connected,
            })
        })
        .collect();
    items.sort_by(|a, b| b.active.cmp(&a.active).then(a.label.cmp(&b.label)));
    items
}

/// Audio outputs from `wpctl status` ("│  *   56. Name [vol: 0.48]").
fn sinks() -> Vec<MenuItem> {
    let Ok(out) = Process::new("wpctl").arg("status").output() else {
        return Vec::new();
    };
    parse_sinks(&String::from_utf8_lossy(&out.stdout))
}

/// The "Sinks:" section of `wpctl status`.
fn parse_sinks(text: &str) -> Vec<MenuItem> {
    let mut in_sinks = false;
    let mut items = Vec::new();
    for line in text.lines() {
        let body = line.trim_start_matches([' ', '│', '├', '└', '─']).trim();
        if body.starts_with("Sinks:") {
            in_sinks = true;
            continue;
        }
        if !in_sinks {
            continue;
        }
        if body.ends_with(':') {
            break; // next section
        }
        let default = body.starts_with('*');
        let body = body.trim_start_matches('*').trim();
        let Some((id, rest)) = body.split_once('.') else {
            continue;
        };
        if id.trim().parse::<u32>().is_err() {
            continue;
        }
        let name = rest.split(" [vol:").next().unwrap_or(rest).trim();
        items.push(MenuItem {
            id: id.trim().to_owned(),
            label: name.to_owned(),
            detail: if default { "Default output" } else { "" }.into(),
            active: default,
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_lines() {
        assert_eq!(parse_volume("Volume: 0.48"), Some((0.48, false)));
        assert_eq!(parse_volume("Volume: 0.30 [MUTED]"), Some((0.30, true)));
        assert_eq!(parse_volume("garbage"), None);
    }

    #[test]
    fn sinks_from_wpctl_status() {
        let status = "Audio
 ├─ Devices:
 │      48. Family 17h HD Audio Controller [alsa]
 │
 ├─ Sinks:
 │  *   56. Speaker + Headphones [vol: 0.48]
 │      61. HDMI / DisplayPort 2 Output [vol: 1.00]
 │
 ├─ Sink endpoints:
 │
 ├─ Sources:
 │  *   57. Microphone [vol: 1.00]
";
        let sinks = parse_sinks(status);
        let got: Vec<(&str, &str, bool)> = sinks
            .iter()
            .map(|s| (s.id.as_str(), s.label.as_str(), s.active))
            .collect();
        assert_eq!(
            got,
            [
                ("56", "Speaker + Headphones", true),
                ("61", "HDMI / DisplayPort 2 Output", false)
            ]
        );
    }
}
