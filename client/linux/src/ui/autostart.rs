//! "Start at login": a desktop entry in ~/.config/autostart (the XDG
//! autostart spec, which GNOME follows).

use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::APP_ID;

fn entry_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join(format!("{APP_ID}.desktop")))
}

pub fn is_enabled() -> bool {
    entry_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .is_some_and(|text| !text.lines().any(|l| l.trim() == "Hidden=true" || l.trim() == "X-GNOME-Autostart-enabled=false"))
}

pub fn set(enabled: bool) -> Result<()> {
    let path = entry_path().context("no home directory")?;
    if !enabled {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    // The installed command, or this binary when run from a build tree.
    let exe = std::env::current_exe().ok();
    let command = match &exe {
        Some(p) if p.starts_with("/usr") => "av-relay".to_string(),
        Some(p) => format!("\"{}\"", p.display()),
        None => "av-relay".to_string(),
    };
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=AV Relay\n\
         Comment=Use your MacBook's camera and microphone\n\
         Exec={command}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    );
    fs::create_dir_all(path.parent().expect("has a parent"))?;
    fs::write(&path, entry).with_context(|| format!("writing {}", path.display()))
}
