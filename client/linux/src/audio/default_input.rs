//! Makes the virtual microphone the default input while the sender is
//! connected, and gives the user's own default back afterwards.
//!
//! The previous default is saved on disk before it is replaced, so a crash or
//! a power cut doesn't leave the user's default pointing at a microphone that
//! no longer exists: the next start puts it back.

use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{MIC_NODE_NAME, PipeWire, is_mic_node};

const STATE_FILE: &str = "default-input.json";

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Saved {
    /// The configured default input before AV Relay took over; `None` means
    /// none was configured (WirePlumber chose one by itself).
    previous: Option<String>,
}

pub struct DefaultInput {
    path: PathBuf,
}

impl DefaultInput {
    /// Keeps its state under $XDG_STATE_HOME/av-relay (~/.local/state/av-relay).
    pub fn new() -> Result<Self> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .context("neither XDG_STATE_HOME nor HOME is set")?;
        Ok(Self::at(base.join("av-relay")))
    }

    fn at(dir: PathBuf) -> Self {
        Self { path: dir.join(STATE_FILE) }
    }

    /// Makes the virtual mic the default input, remembering the current one.
    pub fn take_over(&self, pw: &PipeWire) -> Result<()> {
        let current = pw.default_source()?;
        // Already ours (e.g. after a crash the saved file still holds the
        // real previous value): keep what was saved.
        if !is_mic_node(current.as_deref()) {
            self.save(&Saved { previous: current })?;
        }
        pw.set_default_source(Some(MIC_NODE_NAME))
    }

    /// Restores the saved default input, unless the user has picked another
    /// one in the meantime. Safe to call when nothing was taken over (at
    /// startup, to repair a crash).
    pub fn give_back(&self, pw: &PipeWire) -> Result<()> {
        let Some(saved) = self.load()? else { return Ok(()) };
        if is_mic_node(pw.default_source()?.as_deref()) {
            pw.set_default_source(saved.previous.as_deref())?;
        }
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn load(&self) -> Result<Option<Saved>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("reading {}", self.path.display()))?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    fn save(&self, saved: &Saved) -> Result<()> {
        let dir = self.path.parent().expect("the state file has a directory");
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(saved)?)?;
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_file_round_trip() {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("av-relay-state-{}", u64::from_le_bytes(b)));
        let d = DefaultInput::at(dir.clone());
        assert_eq!(d.load().unwrap(), None);
        d.save(&Saved { previous: Some("alsa_input.x".into()) }).unwrap();
        assert_eq!(d.load().unwrap(), Some(Saved { previous: Some("alsa_input.x".into()) }));
        d.save(&Saved { previous: None }).unwrap();
        assert_eq!(d.load().unwrap(), Some(Saved { previous: None }));
        fs::remove_dir_all(dir).unwrap();
    }
}
