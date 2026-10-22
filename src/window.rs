use gdk4_x11::X11Surface;
use gtk::prelude::*;
use gtk::{gdk, Application, ApplicationWindow, CssProvider};

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
/// of the primary monitor. Returns false if the X11 setup failed.
pub fn show(
    app: &Application,
    child: &impl IsA<gtk::Widget>,
    hit: island::HitRect,
    hover: async_channel::Sender<bool>,
) -> bool {
    load_css();

    // Input outside the notch is excluded by the island's input region.
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Atoll")
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
        app.quit();
        return false;
    };

    let conn = match x11rb::connect(None) {
        Ok((conn, _)) => conn,
        Err(e) => {
            tracing::error!("cannot connect to X server: {e}");
            app.quit();
            return false;
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
        .name("x11".into())
        .spawn(move || x_events(&conn, xid, &hit, &hover));
    if let Err(e) = spawned {
        tracing::error!("cannot start X event thread: {e}");
    }
    true
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
    hit: &island::HitRect,
    hover: &async_channel::Sender<bool>,
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

    let mut target = target_position(conn);
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
                    if hover.send_blocking(inside).is_err() {
                        return; // UI is gone; we're shutting down.
                    }
                }
            }
            // One layout change sends a burst of these; moving is idempotent,
            // and only an actual change of position does anything.
            Ok(Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_)) => {
                let now = target_position(conn);
                if now != target {
                    target = now;
                    move_to(conn, xid, target);
                }
            }
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
        mask: vec![xinput::XIEventMask::RAW_MOTION],
    };
    conn.xinput_xi_select_events(root, &[mask])?.check()?;
    Ok(())
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

/// Top center of the primary monitor, in root-window coordinates.
fn target_position(conn: &impl Connection) -> (i32, i32) {
    let root = conn.setup().roots[0].root;
    let (mx, my, mw) = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| {
            let m = r
                .monitors
                .iter()
                .find(|m| m.primary)
                .or(r.monitors.first())?;
            Some((i32::from(m.x), i32::from(m.y), i32::from(m.width)))
        })
        .unwrap_or_else(|| {
            tracing::warn!("RandR monitor query failed, assuming 1920px at 0,0");
            (0, 0, 1920)
        });
    let x = mx + (mw - island::WIN_W) / 2;
    tracing::info!("placing pill at {x},{my} (monitor width {mw})");
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

fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    match gdk::Display::default() {
        Some(display) => gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        ),
        None => tracing::error!("no display; CSS not loaded"),
    }
}
