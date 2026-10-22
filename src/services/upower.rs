//! Charger plugged/unplugged, from UPower's aggregate "display device".
//! Only transitions are reported; the state at startup is not a peek.

use futures_util::StreamExt;
use zbus::{proxy, Connection};

#[proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait Device {
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;
}

pub enum Update {
    Plugged { percent: f64 },
    Unplugged { percent: f64 },
}

/// UPower states that mean "on external power": charging, fully charged,
/// pending charge.
fn on_power(state: u32) -> bool {
    matches!(state, 1 | 4 | 5)
}

pub fn spawn(updates: async_channel::Sender<Update>) {
    let spawned = std::thread::Builder::new()
        .name("upower".into())
        .spawn(move || {
            if let Err(e) = async_io::block_on(run(&updates)) {
                // No UPower or no battery (desktop): charging peeks stay off.
                tracing::debug!("charging peeks disabled: {e}");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start UPower thread: {e}");
    }
}

async fn run(updates: &async_channel::Sender<Update>) -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let dev = DeviceProxy::new(&conn).await?;
    if !dev.is_present().await? {
        tracing::debug!("no battery; charging peeks disabled");
        return Ok(());
    }
    let mut plugged = on_power(dev.state().await?);
    let mut changes = dev.receive_state_changed().await;
    while let Some(change) = changes.next().await {
        let Ok(state) = change.get().await else {
            continue;
        };
        let now = on_power(state);
        // Charging -> fully charged is still plugged in; not worth a peek.
        if now == plugged {
            continue;
        }
        plugged = now;
        let percent = dev.percentage().await.unwrap_or(0.0);
        tracing::debug!("power changed: plugged={plugged} at {percent}%");
        let update = if plugged {
            Update::Plugged { percent }
        } else {
            Update::Unplugged { percent }
        };
        if updates.send(update).await.is_err() {
            break;
        }
    }
    Ok(())
}
