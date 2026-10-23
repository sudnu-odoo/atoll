mod app;
mod banners;
mod cards;
mod config;
mod hook;
mod island;
mod palette;
mod services;
mod settings;
mod shelf;
mod shell;
mod store;
mod theme;
mod todo;
mod water;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib, Application};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

const APP_ID: &str = "dev.atoll.Atoll";

fn main() -> glib::ExitCode {
    // `atoll hook <event>` is Claude Code's hook helper: no GTK, and no
    // logging, because stdout carries the JSON answer back to Claude Code.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("shell-init") {
        shell::print_init();
        return glib::ExitCode::SUCCESS;
    }
    if args.get(1).map(String::as_str) == Some("hook") {
        hook::run(
            args.get(2).map_or("", String::as_str),
            args.get(3..).unwrap_or(&[]),
        );
        return glib::ExitCode::SUCCESS;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "atoll=info".into()),
        )
        .init();

    // Override-redirect only exists on X11; force the backend so a stray
    // WAYLAND_DISPLAY can't silently put us somewhere the strategy fails.
    std::env::set_var("GDK_BACKEND", "x11");
    // A small 2D notch gains nothing from a GL context, which costs ~40 MB.
    // Still overridable for debugging rendering issues.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }

    // Bundled icons (see resources/); GtkApplication adds them to the icon
    // theme under the app's resource path.
    if let Err(e) = gio::resources_register_include!("atoll.gresource") {
        tracing::warn!("bundled icons unavailable: {e}");
    }

    // Launching `atoll` again (e.g. `atoll --settings`) forwards the command
    // line to the running instance instead of starting a second island.
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_activate(app::build);
    app.connect_command_line(|app, cmdline| {
        app.activate();
        if cmdline.arguments().iter().any(|a| a == "--settings") {
            app::open_settings();
        }
        glib::ExitCode::SUCCESS
    });
    watch_signals();
    let code = app.run();
    // Clean exit (incl. SIGINT/SIGTERM, which quit through GTK): give GNOME
    // its banners back if Atoll had hidden them. Not from a second launch
    // (`atoll --settings`), which only forwarded to the running instance.
    // (Asking GTK `is_remote` here is invalid: the app is unregistered by
    // now.)
    if app::is_primary() {
        banners::restore();
    }
    code
}

/// Quits through GTK on SIGINT/SIGTERM so the window is unmapped cleanly.
/// The thread blocks in the kernel, so it costs nothing while idle.
fn watch_signals() {
    let mut signals = match Signals::new([SIGINT, SIGTERM]) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("cannot install signal handlers: {e}");
            return;
        }
    };
    std::thread::spawn(move || {
        // Keep listening: a handled-but-unread signal would otherwise be
        // swallowed and Atoll would ignore every later SIGTERM.
        for (count, sig) in signals.forever().enumerate() {
            if count > 0 {
                // The graceful quit didn't happen: leave anyway, but never
                // with GNOME's banners still hidden.
                tracing::warn!("signal {sig} again, exiting now");
                banners::restore();
                std::process::exit(0);
            }
            tracing::info!("signal {sig} received, quitting");
            // GTK objects live on the main thread; hop back there to quit.
            glib::MainContext::default().invoke(|| match gio::Application::default() {
                Some(app) => app.quit(),
                None => {
                    banners::restore();
                    std::process::exit(0);
                }
            });
        }
    });
}
