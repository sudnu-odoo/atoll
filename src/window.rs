use gdk4_x11::X11Surface;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow};

use crate::island;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, EventMask,
    InputFocus, StackMode,
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
/// - A fullscreen window on this monitor (video, game, slides) hides the
///   island: we watch the active window and its _NET_WM_STATE.
fn x_events(
    conn: &impl Connection,
    xid: u32,
    connector: &str,
    hit: &island::HitRect,
    pointer: &async_channel::Sender<island::Pointer>,
) {
    let atoms = match Atoms::new(conn) {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::warn!("cannot intern atoms, fullscreen detection off: {e}");
            None
        }
    };
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
    // Our own event mask on the root: tells us when the active window changes.
    let root_mask = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
    if let Err(e) = conn.change_window_attributes(root, &root_mask).map(|_| ()) {
        tracing::warn!("cannot watch the active window: {e}");
    }

    let mut monitor = monitor_rect(conn, connector);
    let mut target = target_of(monitor, connector);
    move_to(conn, xid, target);
    let mut inside = false;
    let mut active: Option<u32> = None;
    let mut fullscreen = false;
    // Re-evaluates fullscreen and tells the UI only when it changes.
    let mut refresh = |active: Option<u32>, monitor: (i32, i32, i32, i32)| -> bool {
        let now = atoms
            .as_ref()
            .zip(active)
            .is_some_and(|(a, w)| is_fullscreen_on(conn, root, a, w, monitor));
        if now == fullscreen {
            return true;
        }
        fullscreen = now;
        tracing::debug!("fullscreen on {connector}: {now}");
        pointer
            .send_blocking(island::Pointer::Fullscreen(now))
            .is_ok()
    };
    if let Some(a) = &atoms {
        active = watch_active(conn, root, a, active);
        refresh(active, monitor);
    }

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
                monitor = monitor_rect(conn, connector);
                let now = target_of(monitor, connector);
                if now != target {
                    target = now;
                    move_to(conn, xid, target);
                }
            }
            Ok(Event::PropertyNotify(ev)) => {
                let Some(a) = &atoms else { continue };
                if ev.window == root && ev.atom == a.active {
                    active = watch_active(conn, root, a, active);
                } else if Some(ev.window) != active || ev.atom != a.state {
                    continue;
                }
                if !refresh(active, monitor) {
                    return;
                }
            }
            // The active window moved (e.g. to another monitor) or resized.
            Ok(Event::ConfigureNotify(ev)) if Some(ev.window) == active => {
                if !refresh(active, monitor) {
                    return;
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

struct Atoms {
    active: u32,
    state: u32,
    fullscreen: u32,
}

impl Atoms {
    fn new(conn: &impl Connection) -> Result<Self, Box<dyn std::error::Error>> {
        let get = |name: &str| -> Result<u32, Box<dyn std::error::Error>> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        Ok(Self {
            active: get("_NET_ACTIVE_WINDOW")?,
            state: get("_NET_WM_STATE")?,
            fullscreen: get("_NET_WM_STATE_FULLSCREEN")?,
        })
    }
}

/// Reads _NET_ACTIVE_WINDOW and subscribes to that window's state and
/// geometry changes. Subscribing to another client's window only adds our
/// own event mask; it doesn't affect that client.
fn watch_active(
    conn: &impl Connection,
    root: u32,
    a: &Atoms,
    previous: Option<u32>,
) -> Option<u32> {
    let reply = conn
        .get_property(false, root, a.active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let window = reply.value32()?.next().filter(|w| *w != 0)?;
    if previous != Some(window) {
        let mask = ChangeWindowAttributesAux::new()
            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY);
        // The window may already be gone; that's fine.
        let _ = conn.change_window_attributes(window, &mask).map(|_| ());
        let _ = conn.flush();
    }
    Some(window)
}

/// Whether `window` is fullscreen and covers part of `monitor`.
fn is_fullscreen_on(
    conn: &impl Connection,
    root: u32,
    a: &Atoms,
    window: u32,
    (mx, my, mw, mh): (i32, i32, i32, i32),
) -> bool {
    let fullscreen = conn
        .get_property(false, window, a.state, AtomEnum::ATOM, 0, 32)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().map(|mut v| v.any(|s| s == a.fullscreen)))
        .unwrap_or(false);
    if !fullscreen {
        return false;
    }
    let Some(geo) = conn.get_geometry(window).ok().and_then(|c| c.reply().ok()) else {
        return false;
    };
    let Some(at) = conn
        .translate_coordinates(window, root, 0, 0)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return false;
    };
    let (x, y) = (i32::from(at.dst_x), i32::from(at.dst_y));
    let (w, h) = (i32::from(geo.width), i32::from(geo.height));
    x < mx + mw && x + w > mx && y < my + mh && y + h > my
}

fn target_of((mx, my, mw, _): (i32, i32, i32, i32), connector: &str) -> (i32, i32) {
    let x = mx + (mw - island::WIN_W) / 2;
    tracing::info!("placing island for {connector} at {x},{my} (monitor width {mw})");
    (x, my)
}

/// Bounds of the monitor named `connector`, in root coordinates. Falls back
/// to the primary (then first) monitor while RandR catches up.
fn monitor_rect(conn: &impl Connection, connector: &str) -> (i32, i32, i32, i32) {
    let root = conn.setup().roots[0].root;
    conn.randr_get_monitors(root, true)
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
            Some((
                i32::from(m.x),
                i32::from(m.y),
                i32::from(m.width),
                i32::from(m.height),
            ))
        })
        .unwrap_or_else(|| {
            tracing::warn!("RandR monitor query failed, assuming 1920x1080 at 0,0");
            (0, 0, 1920, 1080)
        })
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

thread_local! {
    // A UI-thread connection for one-shot focus requests (the event thread's
    // connection is busy blocking in wait_for_event).
    static FOCUS_CONN: Option<x11rb::rust_connection::RustConnection> =
        x11rb::connect(None).ok().map(|(c, _)| c);
}

/// Gives the island's window keyboard focus so the user can type in it.
/// Override-redirect windows get no focus from the window manager, so we
/// set it ourselves. Returns the window that had focus, to hand it back.
pub fn take_focus(widget: &impl IsA<gtk::Widget>) -> Option<u32> {
    let xid = widget
        .native()
        .and_then(|n| n.surface())
        .and_then(|s| s.downcast::<X11Surface>().ok())
        .map(|s| s.xid() as u32)?;
    FOCUS_CONN.with(|conn| {
        let conn = conn.as_ref()?;
        let previous = conn.get_input_focus().ok()?.reply().ok()?.focus;
        conn.set_input_focus(InputFocus::PARENT, xid, x11rb::CURRENT_TIME)
            .ok()?;
        conn.flush().ok()?;
        Some(previous)
    })
}

/// Returns keyboard focus to the window that had it before `take_focus`.
pub fn give_back_focus(previous: u32) {
    FOCUS_CONN.with(|conn| {
        let Some(conn) = conn.as_ref() else { return };
        // The window may have closed meanwhile; then the WM picks one.
        if conn
            .set_input_focus(InputFocus::PARENT, previous, x11rb::CURRENT_TIME)
            .is_ok()
        {
            let _ = conn.flush();
        }
    });
}
