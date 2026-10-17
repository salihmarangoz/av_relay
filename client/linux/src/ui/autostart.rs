//! "Start at login" (the XDG autostart spec, which GNOME follows). The
//! package installs a system-wide entry, so AV Relay starts at every login
//! from the moment it's installed; the switch writes or removes a user
//! entry that overrides it. Run from a build, it manages a user entry.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::APP_ID;

fn entry_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join(format!("{APP_ID}.desktop")))
}

/// The package's entry: starts AV Relay at every login. A user entry with
/// the same name overrides it (XDG autostart spec).
const SYSTEM_ENTRY: &str = "/etc/xdg/autostart/io.github.salihmarangoz.avrelay.desktop";

fn disabled(text: &str) -> bool {
    text.lines().any(|l| matches!(l.trim(), "Hidden=true" | "X-GNOME-Autostart-enabled=false"))
}

pub fn is_enabled() -> bool {
    entry_path().is_some_and(|p| is_enabled_at(Path::new(SYSTEM_ENTRY), &p))
}

pub fn set(enabled: bool) -> Result<()> {
    set_at(Path::new(SYSTEM_ENTRY), &entry_path().context("no home directory")?, enabled)
}

fn is_enabled_at(system: &Path, path: &Path) -> bool {
    match fs::read_to_string(path) {
        Ok(text) => !disabled(&text),
        Err(_) => system.exists(),
    }
}

fn set_at(system: &Path, path: &Path, enabled: bool) -> Result<()> {
    let system = system.exists();
    let remove = || match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(anyhow::Error::from(e)),
        _ => Ok(()),
    };
    let entry = match (enabled, system) {
        // The package's entry starts it; drop any override.
        (true, true) => return remove(),
        (false, false) => return remove(),
        // Override the package's entry for this user.
        (false, true) => "[Desktop Entry]\nType=Application\nName=AV Relay\nHidden=true\n".to_string(),
        // Run from a build: an entry of our own.
        (true, false) => {
            let exe = std::env::current_exe().ok();
            let command = match &exe {
                Some(p) if p.starts_with("/usr") => "av-relay".to_string(),
                Some(p) => format!("\"{}\"", p.display()),
                None => "av-relay".to_string(),
            };
            format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=AV Relay\n\
                 Comment=Use another computer's camera and microphone\n\
                 Exec={command}\n\
                 Icon={APP_ID}\n\
                 Terminal=false\n\
                 X-GNOME-Autostart-enabled=true\n"
            )
        }
    };
    fs::create_dir_all(path.parent().expect("has a parent"))?;
    fs::write(path, entry).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_entry_overrides_the_system_one() {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("av-relay-autostart-{}", u64::from_le_bytes(b)));
        fs::create_dir_all(&dir).unwrap();
        let (system, user) = (dir.join("system.desktop"), dir.join("autostart/user.desktop"));

        // Run from a build: no system entry; on writes, off removes.
        assert!(!is_enabled_at(&system, &user));
        set_at(&system, &user, true).unwrap();
        assert!(is_enabled_at(&system, &user));
        set_at(&system, &user, false).unwrap();
        assert!(!user.exists() && !is_enabled_at(&system, &user));

        // Installed: the system entry is on until the user turns it off.
        fs::write(&system, "[Desktop Entry]\nExec=av-relay\n").unwrap();
        assert!(is_enabled_at(&system, &user));
        set_at(&system, &user, false).unwrap();
        assert!(fs::read_to_string(&user).unwrap().contains("Hidden=true"));
        assert!(!is_enabled_at(&system, &user));
        set_at(&system, &user, true).unwrap();
        assert!(!user.exists() && is_enabled_at(&system, &user));
        fs::remove_dir_all(dir).unwrap();
    }
}
