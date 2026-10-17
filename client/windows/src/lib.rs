// SPDX-License-Identifier: GPL-3.0-or-later
// These modules are compiled from the same files as the Ubuntu receiver.
// In particular, Windows runs the same crypto and wire-vector tests.
#[path = "../../linux/src/crypto/mod.rs"]
pub mod crypto;
#[path = "../../linux/src/app/discovery.rs"]
pub mod discovery;
#[path = "../../linux/src/proto/mod.rs"]
pub mod proto;

#[cfg(feature = "media")]
pub mod app;
#[cfg(feature = "windows-audio")]
pub mod audio;
pub mod autostart;
pub mod icons;
#[cfg(feature = "media")]
#[path = "../../linux/src/media/mod.rs"]
pub mod media;
pub mod store;
#[cfg(feature = "media")]
pub mod ui;
pub mod vcam;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
