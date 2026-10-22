mod app;
mod cards;
mod config;
mod island;
mod palette;
mod services;
mod settings;
mod store;
mod theme;
mod water;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib, Application};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

const APP_ID: &str = "dev.atoll.Atoll";

fn main() -> glib::ExitCode {
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
    app.run()
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
        if let Some(sig) = signals.forever().next() {
            tracing::info!("signal {sig} received, quitting");
            // GTK objects live on the main thread; hop back there to quit.
            glib::MainContext::default().invoke(|| {
                if let Some(app) = gio::Application::default() {
                    app.quit();
                }
            });
        }
    });
}
