// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether an app is recording from the cable's capture side ("CABLE
//! Output"): the microphone switch makes the sender's microphone available,
//! and this decides when it actually captures, as OBS reader handles do for
//! the camera.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use windows::Win32::{
    Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
    Media::Audio::{
        AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionManager2, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
    },
    System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize, STGM_READ, StructuredStorage::PropVariantToStringAlloc,
    },
};

const POLL: Duration = Duration::from_millis(500);

pub struct MicUse {
    in_use: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl MicUse {
    /// Watches the capture endpoint paired with `playback` (the cable's
    /// input, e.g. "CABLE Input (VB-Audio Virtual Cable)"). Without one, the
    /// microphone counts as always in use, as before.
    pub fn start(playback: &str) -> Self {
        let in_use = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let wanted = capture_name(playback);
        let (flag, stopped) = (in_use.clone(), stop.clone());
        let thread = thread::Builder::new()
            .name("microphone-use".into())
            .spawn(move || unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let mut device = None;
                let mut warned = false;
                while !stopped.load(Ordering::Acquire) {
                    if device.is_none() {
                        device = find_capture(&wanted);
                    }
                    let used = match device.as_ref().map(recording) {
                        Some(Ok(used)) => used,
                        other => {
                            if !warned {
                                log::warn!(
                                    "can't tell whether an app records from {wanted} ({:?}); the microphone stays on",
                                    other.map(|r| r.err())
                                );
                                warned = true;
                            }
                            device = None;
                            true
                        }
                    };
                    if flag.swap(used, Ordering::AcqRel) != used {
                        log::info!("{wanted} {}", if used { "in use" } else { "no longer in use" });
                    }
                    thread::sleep(POLL);
                }
                CoUninitialize();
            })
            .ok();
        Self { in_use, stop, thread }
    }
    pub fn in_use(&self) -> bool {
        self.in_use.load(Ordering::Acquire)
    }
}
impl Drop for MicUse {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// VB-CABLE names its two ends "CABLE Input" (playback) and "CABLE
/// Output" (capture), with the same suffix.
fn capture_name(playback: &str) -> String {
    if playback.contains("Input") {
        playback.replacen("Input", "Output", 1)
    } else {
        "CABLE Output".into()
    }
}

fn find_capture(wanted: &str) -> Option<IMMDevice> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE).ok()?;
        let mut fallback = None;
        for i in 0..devices.GetCount().ok()? {
            let Ok(device) = devices.Item(i) else { continue };
            let Some(name) = friendly_name(&device) else { continue };
            if name == wanted {
                return Some(device);
            }
            if fallback.is_none() && name.starts_with(wanted) {
                fallback = Some(device);
            }
        }
        fallback
    }
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let text = PropVariantToStringAlloc(&value).ok()?;
        let name = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        name
    }
}

/// Whether any app has an active recording session on the endpoint.
fn recording(device: &IMMDevice) -> windows::core::Result<bool> {
    unsafe {
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let sessions = manager.GetSessionEnumerator()?;
        for i in 0..sessions.GetCount()? {
            if sessions.GetSession(i)?.GetState()? == AudioSessionStateActive {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_end_of_the_cable() {
        assert_eq!(
            capture_name("CABLE Input (VB-Audio Virtual Cable)"),
            "CABLE Output (VB-Audio Virtual Cable)"
        );
        assert_eq!(capture_name("Speakers"), "CABLE Output");
    }
}
