//! Start with the session: a desktop entry in ~/.config/autostart, the
//! freedesktop way GNOME reads at login (a user file, no dconf involved).
//! The file follows the Settings switch, and is refreshed on every start so
//! it always points at the binary actually in use.

use std::path::PathBuf;

const FILE: &str = "atoll.desktop";

fn entry_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join(FILE))
}

/// The installed binary: the one running now, or, from a development
/// build, an installed copy (package or install.sh). `None` when only a
/// development build exists, so a login never starts a stale debug build.
pub fn installed() -> Option<PathBuf> {
    let current = std::env::current_exe().ok();
    if current
        .as_ref()
        .is_some_and(|p| !p.components().any(|c| c.as_os_str() == "target"))
    {
        return current;
    }
    let user = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin/atoll"));
    user.into_iter()
        .chain([PathBuf::from("/usr/bin/atoll")])
        .find(|p| p.is_file())
}

fn entry(exec: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Atoll\n\
         Comment=Dynamic Island for the GNOME top bar\n\
         Exec=\"{}\"\n\
         Icon=atoll\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n\
         X-GNOME-Autostart-Delay=3\n",
        exec.display()
    )
}

pub fn apply(on: bool) {
    // Tests must never touch the user's login items.
    if cfg!(test) {
        return;
    }
    let Some(path) = entry_path() else { return };
    if on {
        let Some(exec) = installed() else {
            tracing::info!("start at login waits until Atoll is installed");
            return;
        };
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, entry(&exec)));
        match result {
            Ok(()) => tracing::debug!("autostart → {}", exec.display()),
            Err(e) => tracing::warn!("cannot write {}: {e}", path.display()),
        }
    } else if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("cannot remove {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::entry;

    #[test]
    fn entry_quotes_exec_and_starts_late() {
        let e = entry(std::path::Path::new("/home/a b/.local/bin/atoll"));
        assert!(e.contains("Exec=\"/home/a b/.local/bin/atoll\"\n"));
        assert!(e.contains("X-GNOME-Autostart-Delay=3"));
        assert!(e.starts_with("[Desktop Entry]\n"));
    }
}
