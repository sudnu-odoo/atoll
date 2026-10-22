use gdk4_x11::X11Surface;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow};

use crate::island;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, EventMask, StackMode,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

/// Shows `child` in a transparent override-redirect window at the top center
/// of the monitor named `connector` (e.g. "DisplayPort-7"). Returns None if
/// the X11 setup failed. Destroying the window ends its X thread.
pub fn show(
    app: &Application,
    child: &impl IsA<gtk::Widget>,
    connector: String,
    hit: island::HitRect,
    pointer: async_channel::Sender<island::Pointer>,
) -> Option<ApplicationWindow> {
    // Input outside the notch is excluded by the island's input region.
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Atoll")
        .css_classes(["island"])
        .decorated(false)
        .resizable(false)
        .default_width(island::WIN_W)
        .default_height(island::WIN_H)
        .child(child)
        .build();

    // override-redirect must be set before the first map, or mutter has
    // already managed (decorated, focused, listed) the window.
    WidgetExt::realize(&window);
    let Some(xid) = window
        .surface()
        .and_then(|s| s.downcast::<X11Surface>().ok())
        .map(|s| s.xid() as u32)
    else {
        tracing::error!("not an X11 surface; Atoll needs an Xorg session");
        return None;
    };

    let conn = match x11rb::connect(None) {
        Ok((conn, _)) => conn,
        Err(e) => {
            tracing::error!("cannot connect to X server: {e}");
            return None;
        }
    };

    if let Err(e) = conn
        .change_window_attributes(xid, &ChangeWindowAttributesAux::new().override_redirect(1))
        .map(|_| ())
    {
        tracing::error!("setting override-redirect failed: {e}");
    }
    // This connection is separate from GTK's, so the attribute must reach the
    // server before GTK maps the window; otherwise mutter manages (frames,
    // focuses) it first. A round-trip guarantees it has been processed.
    if let Err(e) = conn.sync() {
        tracing::error!("X sync failed: {e}");
    }

    window.present();
    let spawned = std::thread::Builder::new()
        .name(format!("x11 {connector}"))
        .spawn(move || x_events(&conn, xid, &connector, &hit, &pointer));
    if let Err(e) = spawned {
        tracing::error!("cannot start X event thread: {e}");
    }
    Some(window)
}

/// Owns all X events for the island, on its own thread. It blocks in the
/// kernel between events, so idle cost is zero.
///
/// - GTK moves the window on its own after mapping, at a timing-dependent
///   moment; rather than racing it, move back on every stray ConfigureNotify.
/// - GNOME Shell receives pointer input over the top bar even though we're
///   drawn above it, so hover is derived from XInput2 raw motion, which is
///   delivered regardless of which window is under the pointer.
/// - Docking, undocking or changing the primary monitor emits RandR events;
///   the island re-centers on the (new) primary monitor.
fn x_events(
    conn: &impl Connection,
    xid: u32,
    connector: &str,
    hit: &island::HitRect,
    pointer: &async_channel::Sender<island::Pointer>,
) {
    let mask = ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY);
    if let Err(e) = conn.change_window_attributes(xid, &mask).map(|_| ()) {
        tracing::error!("cannot watch window moves: {e}");
    }
    let root = conn.setup().roots[0].root;
    if let Err(e) = select_raw_motion(conn, root) {
        tracing::warn!("XInput2 unavailable, hover over the top bar won't work: {e}");
    }

    if let Err(e) = select_monitor_changes(conn, root) {
        tracing::warn!("RandR events unavailable, won't follow monitor changes: {e}");
    }

    let mut target = target_position(conn, connector);
    move_to(conn, xid, target);
    let mut inside = false;

    loop {
        match conn.wait_for_event() {
            Ok(Event::ConfigureNotify(ev)) if ev.window == xid => {
                let at = (i32::from(ev.x), i32::from(ev.y));
                if at != target {
                    tracing::debug!("window moved to {at:?}, moving back");
                    move_to(conn, xid, target);
                }
            }
            Ok(Event::XinputRawMotion(_)) => {
                let Some(now) = pointer_in(conn, root, target, hit) else {
                    continue;
                };
                if now != inside {
                    inside = now;
                    tracing::debug!("hover {inside}");
                    if pointer
                        .send_blocking(island::Pointer::Hover(inside))
                        .is_err()
                    {
                        return; // UI is gone; we're shutting down.
                    }
                }
            }
            // Clicks over the GNOME bar never reach GTK (the Shell takes
            // them), so report primary-button presses on the notch ourselves.
            Ok(Event::XinputRawButtonPress(ev)) if ev.detail == 1 && inside => {
                let Some((x, y)) = pointer_at(conn, root, target) else {
                    continue;
                };
                if pointer.send_blocking(island::Pointer::Click(x, y)).is_err() {
                    return;
                }
            }
            // One layout change sends a burst of these; moving is idempotent,
            // and only an actual change of position does anything.
            Ok(Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_)) => {
                let now = target_position(conn, connector);
                if now != target {
                    target = now;
                    move_to(conn, xid, target);
                }
            }
            // The monitor went away and the UI destroyed our window.
            Ok(Event::DestroyNotify(ev)) if ev.window == xid => return,
            Ok(_) => {}
            Err(e) => {
                // The X connection is gone (logout, X restart): nothing to do.
                tracing::warn!("X connection closed: {e}");
                return;
            }
        }
    }
}

fn select_monitor_changes(
    conn: &impl Connection,
    root: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::protocol::randr::NotifyMask;
    conn.randr_query_version(1, 5)?.reply()?;
    let mask = NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE;
    conn.randr_select_input(root, mask)?.check()?;
    Ok(())
}

fn select_raw_motion(conn: &impl Connection, root: u32) -> Result<(), Box<dyn std::error::Error>> {
    // Raw events need XI 2.1+; the server must be told which version we speak.
    conn.xinput_xi_query_version(2, 2)?.reply()?;
    let mask = xinput::EventMask {
        deviceid: xinput::Device::ALL_MASTER.into(),
        mask: vec![xinput::XIEventMask::RAW_MOTION | xinput::XIEventMask::RAW_BUTTON_PRESS],
    };
    conn.xinput_xi_select_events(root, &[mask])?.check()?;
    Ok(())
}

/// Pointer position relative to the island window.
fn pointer_at(conn: &impl Connection, root: u32, (wx, wy): (i32, i32)) -> Option<(i32, i32)> {
    let p = conn.query_pointer(root).ok()?.reply().ok()?;
    Some((i32::from(p.root_x) - wx, i32::from(p.root_y) - wy))
}

/// Whether the pointer is over the notch. Raw events carry no position, so ask.
fn pointer_in(
    conn: &impl Connection,
    root: u32,
    (wx, wy): (i32, i32),
    hit: &island::HitRect,
) -> Option<bool> {
    let (x, y, w, h) = *hit.lock().ok()?;
    let p = conn.query_pointer(root).ok()?.reply().ok()?;
    let (px, py) = (i32::from(p.root_x) - wx, i32::from(p.root_y) - wy);
    Some(px >= x && px < x + w && py >= y && py < y + h)
}

/// Top center of the monitor named `connector`, in root coordinates. Falls
/// back to the primary (then first) monitor while RandR catches up.
fn target_position(conn: &impl Connection, connector: &str) -> (i32, i32) {
    let root = conn.setup().roots[0].root;
    let (mx, my, mw) = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| {
            let named = r.monitors.iter().find(|m| {
                conn.get_atom_name(m.name)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .is_some_and(|a| a.name == connector.as_bytes())
            });
            let m = named
                .or_else(|| r.monitors.iter().find(|m| m.primary))
                .or(r.monitors.first())?;
            Some((i32::from(m.x), i32::from(m.y), i32::from(m.width)))
        })
        .unwrap_or_else(|| {
            tracing::warn!("RandR monitor query failed, assuming 1920px at 0,0");
            (0, 0, 1920)
        });
    let x = mx + (mw - island::WIN_W) / 2;
    tracing::info!("placing island for {connector} at {x},{my} (monitor width {mw})");
    (x, my)
}

/// Override-redirect windows are never placed by the WM, so we do it ourselves.
fn move_to(conn: &impl Connection, xid: u32, (x, y): (i32, i32)) {
    let aux = ConfigureWindowAux::new()
        .x(x)
        .y(y)
        .stack_mode(StackMode::ABOVE);
    if let Err(e) = conn.configure_window(xid, &aux).map(|_| ()) {
        tracing::error!("configure_window failed: {e}");
    }
    if let Err(e) = conn.flush() {
        tracing::error!("X flush failed: {e}");
    }
}
