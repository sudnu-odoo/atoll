//! Bluetooth device connections, from BlueZ property changes. Only new
//! connections are reported; devices already connected at startup are not.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use zbus::fdo::PropertiesProxy;
use zbus::message::Type;
use zbus::names::InterfaceName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, MatchRule, MessageStream};

const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";
/// Headsets report battery a moment after connecting.
const BATTERY_WAIT: Duration = Duration::from_millis(1500);

pub struct Connected {
    pub name: String,
    /// Headphones and headsets get a headphones badge.
    pub audio: bool,
    pub battery: Option<u8>,
}

pub fn spawn(updates: async_channel::Sender<Connected>) {
    let spawned = std::thread::Builder::new()
        .name("bluez".into())
        .spawn(move || {
            if let Err(e) = async_io::block_on(run(&updates)) {
                // No BlueZ (no adapter, or service off): peeks stay off.
                tracing::debug!("bluetooth peeks disabled: {e}");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start BlueZ thread: {e}");
    }
}

async fn run(updates: &async_channel::Sender<Connected>) -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.bluez")?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .build();
    let mut signals = MessageStream::for_match_rule(rule, &conn, None).await?;

    while let Some(msg) = signals.next().await {
        let Ok(msg) = msg else { continue };
        let Some(path) = msg
            .header()
            .path()
            .map(|p| OwnedObjectPath::from(p.to_owned()))
        else {
            continue;
        };
        let Ok((iface, changed, _)) = msg
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        else {
            continue;
        };
        let connected = iface == DEVICE
            && changed
                .get("Connected")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false);
        if !connected {
            continue;
        }

        // One unreadable device must not end the service.
        let Some(device) = describe(&conn, path).await else {
            continue;
        };
        tracing::debug!(
            "bluetooth connected: {} (battery {:?})",
            device.name,
            device.battery
        );
        if updates.send(device).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn describe(conn: &Connection, path: OwnedObjectPath) -> Option<Connected> {
    let props = PropertiesProxy::builder(conn)
        .destination("org.bluez")
        .ok()?
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?;
    let name = string(&props, DEVICE, "Alias")
        .await
        .unwrap_or_else(|| "Bluetooth device".into());
    let audio = string(&props, DEVICE, "Icon")
        .await
        .is_some_and(|icon| icon.starts_with("audio-"));
    async_io::Timer::after(BATTERY_WAIT).await;
    let battery = props
        .get(
            InterfaceName::from_static_str_unchecked(BATTERY),
            "Percentage",
        )
        .await
        .ok()
        .and_then(|v| u8::try_from(v).ok());
    Some(Connected {
        name,
        audio,
        battery,
    })
}

async fn string(props: &PropertiesProxy<'_>, iface: &'static str, prop: &str) -> Option<String> {
    let v = props
        .get(InterfaceName::from_static_str_unchecked(iface), prop)
        .await
        .ok()?;
    String::try_from(v).ok()
}
