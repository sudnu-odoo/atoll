//! Updates from the project's GitHub Releases: is there a newer version,
//! what changed, and installing its .deb. Network work runs on a thread;
//! results come back through a channel (never GTK from here).
//!
//! Installing a package needs root: it goes through `pkexec`, the system's
//! own password prompt, and only when the user clicks Update. Atoll itself
//! stays an ordinary user process.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// The version this build is. CI stamps the release version in
/// (`ATOLL_VERSION`); local builds use Cargo.toml's.
pub const VERSION: &str = match option_env!("ATOLL_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

const TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = concat!("Atoll/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug)]
pub struct Release {
    /// "0.1.4", without the leading "v".
    pub version: String,
    /// What changed, as written in the release.
    pub notes: String,
    pub deb_url: String,
}

#[derive(Clone, Debug)]
pub enum Status {
    UpToDate,
    Available(Release),
    /// Couldn't reach GitHub or read its answer.
    Failed(String),
}

/// "owner/name", from Cargo.toml's `repository`.
fn repo() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/")
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

fn client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
}

/// Asks GitHub for the latest release (blocking: call from a thread).
pub fn check() -> Status {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let body = client()
        .and_then(|c| c.get(&url).send())
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes());
    match body
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<GhRelease>(&b).map_err(|e| e.to_string()))
    {
        Ok(r) => status_of(r, VERSION),
        Err(e) => Status::Failed(e),
    }
}

fn status_of(r: GhRelease, current: &str) -> Status {
    let version = r.tag_name.trim_start_matches('v').to_owned();
    if !newer(&version, current) {
        return Status::UpToDate;
    }
    let deb = r
        .assets
        .into_iter()
        .find(|a| a.name.ends_with("_amd64.deb"));
    match deb {
        Some(deb) => Status::Available(Release {
            version,
            notes: r.body.unwrap_or_default().trim().to_owned(),
            deb_url: deb.browser_download_url,
        }),
        None => Status::Failed(format!("release {version} has no .deb")),
    }
}

/// Whether `a` is a later version than `b` ("0.1.10" > "0.1.9").
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| {
                p.trim_matches(|c: char| !c.is_ascii_digit())
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    parts(a) > parts(b)
}

/// Downloads the release's .deb and installs it with the system's password
/// prompt. Blocking: call from a thread. `Ok` means installed.
pub fn install(release: &Release) -> Result<(), String> {
    let dir = cache_dir().ok_or("no cache folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("atoll_{}_amd64.deb", release.version));
    let bytes = client()
        .and_then(|c| c.get(&release.deb_url).send())
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("download failed: {e}"))?;
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let status = std::process::Command::new("pkexec")
        .args(["apt-get", "install", "-y", "--allow-downgrades"])
        .arg(&path)
        .status()
        .map_err(|e| format!("cannot ask for the password: {e}"))?;
    // The downloaded file is no longer needed either way.
    let _ = std::fs::remove_file(&path);
    if status.success() {
        Ok(())
    } else {
        Err("not installed (cancelled or failed)".into())
    }
}

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|p| p.join("atoll"))
}

/// Restarts Atoll when its installed program file is replaced: an update
/// through App Center, a double-clicked .deb, apt, or Settings → Updates.
/// The new version takes over without anyone having to restart it.
/// Development builds (under `target/`) aren't watched.
pub fn restart_when_replaced() {
    use gtk::gio;
    use gtk::prelude::*;
    let Some(path) = crate::autostart::installed() else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if exe != path {
        return; // running from somewhere else (a dev build)
    }
    let stamp = |p: &std::path::Path| {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).ok().map(|m| (m.ino(), m.mtime()))
    };
    let started_with = stamp(&path);
    let file = gio::File::for_path(&path);
    let Ok(monitor) = file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
    else {
        return;
    };
    let pending = std::rc::Rc::new(std::cell::Cell::new(false));
    monitor.connect_changed(move |_, _, _, _| {
        // dpkg writes the new file, then swaps it in; wait until it settles.
        if pending.replace(true) {
            return;
        }
        let (pending, path) = (pending.clone(), path.clone());
        gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
            pending.set(false);
            let now = stamp(&path);
            if now.is_none() || now == started_with {
                return; // removed (uninstall) or not really changed
            }
            tracing::info!("Atoll was updated; restarting into the new version");
            restart_after_quit();
            if let Some(app) = gio::Application::default() {
                app.quit();
            }
        });
    });
    // The monitor lives as long as the app.
    std::mem::forget(monitor);
}

/// Starts the freshly installed Atoll once this one has quit (the new one
/// can't start while this one still holds the app's name).
pub fn restart_after_quit() {
    let _ = std::process::Command::new("sh")
        .args(["-c", "sleep 1; exec atoll"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, deb: bool) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            body: Some("- Fix things\n".into()),
            assets: if deb {
                vec![GhAsset {
                    name: format!("atoll_{}_amd64.deb", tag.trim_start_matches('v')),
                    browser_download_url: "https://example.com/a.deb".into(),
                }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn latest_release_becomes_a_status() {
        assert!(matches!(
            status_of(release("v0.1.0", true), "0.1.0"),
            Status::UpToDate
        ));
        match status_of(release("v0.1.4", true), "0.1.0") {
            Status::Available(r) => {
                assert_eq!(r.version, "0.1.4");
                assert_eq!(r.notes, "- Fix things");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            status_of(release("v0.2.0", false), "0.1.0"),
            Status::Failed(_)
        ));
    }

    #[test]
    fn repo_comes_from_cargo() {
        assert_eq!(repo(), "sudnu-odoo/atoll");
    }
}
