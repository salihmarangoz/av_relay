//! What the client keeps on disk, under $XDG_CONFIG_HOME/av-relay
//! (~/.config/av-relay): its identity key (readable only by the user) and
//! the paired sender (docs/protocol.md, section 2).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::crypto::{self, StaticKey};
use crate::proto::VideoFormat;

const IDENTITY_FILE: &str = "identity.key";
const PAIRED_FILE: &str = "paired.json";
const SETTINGS_FILE: &str = "settings.json";
const LEARNED_FILE: &str = "learned.json";

/// What AV Relay learned about this computer's network, to start the next
/// session with.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Learned {
    /// The playout delay the last session ended with.
    pub playout_delay_ms: Option<u32>,
}

/// The user's choices.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    /// The virtual camera's format: the most the sender sends.
    pub video: VideoFormat,
    /// Set once the first start has turned "start at login" on, so turning
    /// it off later sticks.
    pub autostart_offered: bool,
    /// Keep the virtual camera and microphone (and the default input) while
    /// the sender is away, instead of removing them after the grace period.
    pub keep_devices: bool,
    /// "Enable AV Relay". Off is as if the app were closed (no connection,
    /// no devices), with the tray icon left to switch it back on.
    pub enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { video: VideoFormat { width: 1920, height: 1080, fps: 30 }, autostart_offered: false, keep_devices: false, enabled: true }
    }
}

pub struct Store {
    dir: PathBuf,
}

/// The paired sender and where it was last reached.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PairedSender {
    /// The sender's static public key, hex.
    pub public_key: String,
    pub device_id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    /// Seconds since the Unix epoch.
    pub paired_at: u64,
}

impl PairedSender {
    pub fn key(&self) -> Result<[u8; 32]> {
        let bytes = hex_decode(&self.public_key).context("the paired sender's key isn't hex")?;
        bytes.try_into().ok().context("the paired sender's key isn't 32 bytes")
    }
}

impl Store {
    /// The store in the user's config directory.
    pub fn open_default() -> Result<Self> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .context("neither XDG_CONFIG_HOME nor HOME is set")?;
        Self::open(base.join("av-relay"))
    }

    pub fn open(dir: PathBuf) -> Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This computer's static key, created on first use.
    pub fn identity(&self) -> Result<StaticKey> {
        let path = self.dir.join(IDENTITY_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                let mode = fs::metadata(&path)?.permissions().mode();
                if mode & 0o077 != 0 {
                    bail!("{} is readable by other users (mode {:o}); fix with chmod 600", path.display(), mode & 0o777);
                }
                let private: [u8; 32] = bytes
                    .try_into()
                    .ok()
                    .with_context(|| format!("{} isn't a 32-byte key", path.display()))?;
                Ok(StaticKey::from_private(private))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = StaticKey::generate();
                // create_new: never overwrite a key another instance just wrote.
                let mut f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .with_context(|| format!("creating {}", path.display()))?;
                f.write_all(key.private())?;
                f.sync_all()?;
                log::info!("created this computer's identity, device ID {}", crypto::device_id(key.public()));
                Ok(key)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn paired_sender(&self) -> Result<Option<PairedSender>> {
        let path = self.dir.join(PAIRED_FILE);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save_paired_sender(&self, sender: &PairedSender) -> Result<()> {
        self.write_atomic(PAIRED_FILE, &serde_json::to_vec_pretty(sender)?)
    }

    pub fn settings(&self) -> Result<Settings> {
        let path = self.dir.join(SETTINGS_FILE);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        self.write_atomic(SETTINGS_FILE, &serde_json::to_vec_pretty(settings)?)
    }

    pub fn learned(&self) -> Learned {
        fs::read(self.dir.join(LEARNED_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save_learned(&self, learned: &Learned) -> Result<()> {
        self.write_atomic(LEARNED_FILE, &serde_json::to_vec_pretty(learned)?)
    }

    pub fn forget_paired_sender(&self) -> Result<()> {
        match fs::remove_file(self.dir.join(PAIRED_FILE)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn write_atomic(&self, name: &str, contents: &[u8]) -> Result<()> {
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
        fs::rename(&tmp, self.dir.join(name))?;
        Ok(())
    }
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let dir = std::env::temp_dir().join(format!("av-relay-test-{}-{}", std::process::id(), rand_suffix()));
        Store::open(dir).unwrap()
    }

    fn rand_suffix() -> u64 {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).unwrap();
        u64::from_le_bytes(b)
    }

    #[test]
    fn identity_is_created_once_and_private() {
        let store = temp_store();
        let a = store.identity().unwrap();
        let b = store.identity().unwrap();
        assert_eq!(a.public(), b.public());
        let mode = fs::metadata(store.dir().join(IDENTITY_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777, 0o700);

        fs::set_permissions(store.dir().join(IDENTITY_FILE), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.identity().is_err(), "a world-readable key must be refused");
        fs::remove_dir_all(store.dir()).unwrap();
    }

    #[test]
    fn paired_mac_round_trip() {
        let store = temp_store();
        assert_eq!(store.paired_sender().unwrap(), None);
        let key = [7u8; 32];
        let sender = PairedSender {
            public_key: hex_encode(&key),
            device_id: crypto::device_id(&key),
            name: "MacBook Air".into(),
            address: "192.0.2.10".into(),
            port: 48100,
            paired_at: 1,
        };
        store.save_paired_sender(&sender).unwrap();
        assert_eq!(store.paired_sender().unwrap(), Some(sender.clone()));
        assert_eq!(sender.key().unwrap(), key);
        store.forget_paired_sender().unwrap();
        assert_eq!(store.paired_sender().unwrap(), None);
        fs::remove_dir_all(store.dir()).unwrap();
    }

    #[test]
    fn settings_default_and_round_trip() {
        let store = temp_store();
        assert_eq!(store.settings().unwrap().video, VideoFormat { width: 1920, height: 1080, fps: 30 });
        let s = Settings { video: VideoFormat { width: 1280, height: 720, fps: 30 }, autostart_offered: true, keep_devices: true, enabled: false };
        store.save_settings(&s).unwrap();
        assert_eq!(store.settings().unwrap(), s);
        fs::remove_dir_all(store.dir()).unwrap();
    }
}
