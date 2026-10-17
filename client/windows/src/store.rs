// SPDX-License-Identifier: GPL-3.0-or-later
//! DPAPI protects the identity even when the profile folder inherits broad ACLs.
use crate::{
    crypto::{self, StaticKey},
    proto::VideoFormat,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedSender {
    pub public_key: String,
    pub device_id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
}
impl PairedSender {
    pub fn key(&self) -> Result<[u8; 32]> {
        let key = decode_key(&self.public_key)?;
        if crypto::device_id(&key) != self.device_id {
            bail!("Stored sender identity is inconsistent; pair again.");
        }
        Ok(key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub video: VideoFormat,
    /// Playback endpoint. Never fall back to the system speakers.
    pub audio_device: Option<String>,
    pub enabled: bool,
    pub start_minimized: bool,
    /// The Camera and Microphone switches: the device is available, and
    /// the sender captures while an app uses it.
    pub camera: bool,
    pub microphone: bool,
}

/// Display names and wire formats for the resolution menu. Add future
/// presets here so the UI and saved-settings validation stay in agreement.
pub const VIDEO_PRESETS: &[(&str, VideoFormat)] = &[
    (
        "720p",
        VideoFormat {
            width: 1280,
            height: 720,
            fps: 30,
        },
    ),
    (
        "1080p",
        VideoFormat {
            width: 1920,
            height: 1080,
            fps: 30,
        },
    ),
];

impl Default for Settings {
    fn default() -> Self {
        Self {
            video: VIDEO_PRESETS[1].1,
            audio_device: None,
            enabled: true,
            start_minimized: true,
            camera: false,
            microphone: false,
        }
    }
}

impl Store {
    pub fn open_default() -> Result<Self> {
        let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
        Self::open(PathBuf::from(base).join("AV Relay"))
    }
    pub fn open(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn identity(&self) -> Result<StaticKey> {
        let path = self.dir.join("identity.dpapi");
        match fs::read(&path) {
            Ok(data) => {
                let private = protect(&data, false)
                    .context("Cannot unlock the identity for this Windows account")?;
                let key: [u8; 32] = private
                    .as_slice()
                    .try_into()
                    .context("Invalid identity length")?;
                Ok(StaticKey::from_private(key))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = StaticKey::generate();
                let encrypted = protect(key.private(), true)?;
                // Never replace a key another process has just created.
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path)?;
                file.write_all(&encrypted)?;
                file.sync_all()?;
                Ok(key)
            }
            Err(e) => Err(e.into()),
        }
    }
    pub fn paired(&self) -> Result<Option<PairedSender>> {
        let sender: Option<PairedSender> = self.read("paired.json")?;
        if let Some(s) = &sender {
            s.key()?;
        }
        Ok(sender)
    }
    pub fn save_paired(&self, sender: &PairedSender) -> Result<()> {
        sender.key()?;
        self.write("paired.json", sender)
    }
    pub fn forget(&self) -> Result<()> {
        match fs::remove_file(self.dir.join("paired.json")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
    pub fn settings(&self) -> Result<Settings> {
        let settings: Settings = self.read("settings.json")?.unwrap_or_default();
        if !VIDEO_PRESETS
            .iter()
            .any(|(_, format)| *format == settings.video)
        {
            bail!("Invalid camera format in settings.json; choose a supported resolution.");
        }
        Ok(settings)
    }
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        self.write("settings.json", settings)
    }
    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        match fs::read(self.dir.join(name)) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).with_context(|| format!("Reading {name}"))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn write(&self, name: &str, value: &impl Serialize) -> Result<()> {
        use std::io::Write;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let temp = self.dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut f = fs::File::create(&temp)?;
        f.write_all(&serde_json::to_vec_pretty(value)?)?;
        f.sync_all()?;
        drop(f);
        let src = crate::wide(&temp.to_string_lossy());
        let dst = crate::wide(&self.dir.join(name).to_string_lossy());
        if unsafe {
            MoveFileExW(
                src.as_ptr(),
                dst.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

fn decode_key(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        bail!("Invalid stored sender key");
    }
    let mut out = [0; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn protect(input: &[u8], encrypt: bool) -> Result<Zeroizing<Vec<u8>>> {
    let blob = CRYPT_INTEGER_BLOB {
        cbData: input.len().try_into()?,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut out: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };
    let result = unsafe {
        if encrypt {
            CryptProtectData(
                &blob,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &blob,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let data = Zeroizing::new(
        unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec(),
    );
    unsafe {
        // Clear decrypted bytes before returning the Windows allocation.
        ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        LocalFree(out.pbData.cast());
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dpapi_identity_and_sender_round_trip() {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!("avrelay-test-{}", u64::from_le_bytes(nonce)));
        let store = Store::open(path.clone()).unwrap();
        let a = store.identity().unwrap();
        assert_eq!(a.public(), store.identity().unwrap().public());
        assert_ne!(fs::read(path.join("identity.dpapi")).unwrap(), a.private());
        let sender = PairedSender {
            public_key: a.public().iter().map(|x| format!("{x:02x}")).collect(),
            device_id: crypto::device_id(a.public()),
            name: "Sender".into(),
            address: "127.0.0.1".into(),
            port: 48100,
        };
        store.save_paired(&sender).unwrap();
        assert_eq!(store.paired().unwrap(), Some(sender));
        store.save_settings(&Settings::default()).unwrap();
        store.save_settings(&Settings::default()).unwrap();
        store.forget().unwrap();
        assert!(store.paired().unwrap().is_none());
        fs::remove_dir_all(path).unwrap();
    }
}
