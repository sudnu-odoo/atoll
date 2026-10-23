//! Local JSON persistence: settings in ~/.config/atoll, data in
//! ~/.local/share/atoll (both honor the XDG variables).

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    // Tests must never read or write the user's real to-dos, shelf, water
    // log or settings: they get a private folder per test run.
    if cfg!(test) {
        let dir = std::env::temp_dir().join(format!("atoll-test-{}", std::process::id()));
        return Some(dir.join(fallback).join("atoll"));
    }
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))?;
    Some(base.join("atoll"))
}

pub fn config_file(name: &str) -> Option<PathBuf> {
    Some(xdg("XDG_CONFIG_HOME", ".config")?.join(name))
}

pub fn data_file(name: &str) -> Option<PathBuf> {
    Some(xdg("XDG_DATA_HOME", ".local/share")?.join(name))
}

/// `Ok(None)` when the file doesn't exist yet; `Err` when it exists but
/// can't be read or parsed (the caller decides whether to fall back).
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Writes via a temp file and rename, so a crash mid-write never leaves a
/// truncated file behind.
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| fail(&e))?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(|e| fail(&e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| fail(&e))?;
    std::fs::rename(&tmp, path).map_err(|e| fail(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_out_of_the_users_home() {
        let p = data_file("x.json").unwrap();
        assert!(p.starts_with(std::env::temp_dir()), "{}", p.display());
    }

    #[test]
    fn write_then_read_back() {
        let p = data_file("store-roundtrip.json").unwrap();
        write(&p, &vec![1, 2, 3]).unwrap();
        assert_eq!(read::<Vec<i32>>(&p).unwrap(), Some(vec![1, 2, 3]));
        assert!(
            !p.with_extension("json.tmp").exists(),
            "no temp file left behind"
        );
    }

    #[test]
    fn missing_is_none_and_broken_is_error() {
        let missing = data_file("store-missing.json").unwrap();
        assert_eq!(read::<Vec<i32>>(&missing).unwrap(), None);
        let broken = data_file("store-broken.json").unwrap();
        std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
        std::fs::write(&broken, b"{ not json").unwrap();
        assert!(read::<Vec<i32>>(&broken).is_err());
    }
}
