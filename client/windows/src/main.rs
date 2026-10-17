// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use anyhow::Result;
use av_relay_windows::{store::Store, wide};
use std::ptr;
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError},
    System::Threading::CreateMutexW,
    UI::WindowsAndMessaging::{
        FindWindowW, MB_ICONERROR, MB_OK, MessageBoxW, PostMessageW, SW_RESTORE,
        SetForegroundWindow, ShowWindow, WM_APP,
    },
};

fn main() {
    if let Err(e) = run() {
        log::error!("{e:#}");
        eprintln!("{e:#}");
        if !std::env::args().any(|a| a == "--doctor" || a == "--discover") {
            unsafe {
                MessageBoxW(
                    ptr::null_mut(),
                    wide(&format!("{e:#}")).as_ptr(),
                    wide("AV Relay").as_ptr(),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    if std::env::args().any(|a| a == "--quit") {
        let window = unsafe { FindWindowW(wide("AVRelayWindowsReceiver").as_ptr(), ptr::null()) };
        if !window.is_null() {
            unsafe {
                PostMessageW(window, WM_APP + 12, 0, 0);
            }
        }
        return Ok(());
    }
    let store = Store::open_default()?;
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store.dir().join("av-relay.log"))?;
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .try_init();
    if std::env::args().any(|a| a == "--doctor") {
        return doctor(&store);
    }
    if std::env::args().any(|a| a == "--discover") {
        let found = av_relay_windows::discovery::browse(std::time::Duration::from_secs(4));
        let report:Vec<_>=found.iter().map(|s|serde_json::json!({"name":s.name,"address":s.address.to_string(),"port":s.port,"device_id":s.device_id})).collect();
        std::fs::write(
            store.dir().join("senders.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let name = wide("Local\\AVRelayWindowsReceiver");
    let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        let window = unsafe { FindWindowW(wide("AVRelayWindowsReceiver").as_ptr(), ptr::null()) };
        if !window.is_null() {
            unsafe {
                ShowWindow(window, SW_RESTORE);
                SetForegroundWindow(window);
            }
            return Ok(());
        }
        anyhow::bail!("AV Relay is starting. Wait a moment, then open it again.");
    }
    struct MutexGuard(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for MutexGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let _guard = MutexGuard(handle);
    #[cfg(feature = "media")]
    {
        let (commands, events, worker) = av_relay_windows::app::start(store.clone())?;
        let at_login = std::env::args().any(|a| a == "--at-login");
        let minimized = (std::env::args().any(|a| a == "--minimized")
            || (at_login && store.settings()?.start_minimized))
            && store.paired()?.is_some();
        let result = av_relay_windows::ui::run(&store, commands.clone(), events, minimized);
        let _ = commands.send(av_relay_windows::app::Command::Quit);
        // Give normal teardown time to close capture and mark the camera
        // queue stopped, without waiting on a pairing timeout during exit.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if worker.is_finished() {
            let _ = worker.join();
        }
        result
    }
    #[cfg(not(feature = "media"))]
    anyhow::bail!(
        "Built without media support. Rebuild with the default features after installing GStreamer."
    )
}
fn doctor(store: &Store) -> Result<()> {
    #[allow(unused_mut)]
    let mut missing = false;
    let mut report = format!(
        "AV Relay Windows {}\nState: {}\nOBS Virtual Camera: {}\n",
        env!("CARGO_PKG_VERSION"),
        store.dir().display(),
        if av_relay_windows::vcam::installed() {
            "registered"
        } else {
            "MISSING: install OBS Studio"
        }
    );
    report.push_str(&format!(
        "Pairing: {}\n",
        store
            .paired()?
            .map(|s| s.name)
            .unwrap_or_else(|| "not paired".into())
    ));
    #[cfg(feature = "media")]
    {
        use anyhow::Context;
        gstreamer::init().context("Initialising GStreamer")?;
        for name in [
            "webrtcbin",
            "nicesrc",
            "nicesink",
            "dtlssrtpdec",
            "rtph264depay",
            "h264parse",
            "avdec_h264",
            "videoconvertscale",
            "rtpopusdepay",
            "opusdec",
            "audioconvert",
            "audioresample",
            "appsink",
        ] {
            let present = gstreamer::ElementFactory::find(name).is_some();
            missing |= !present;
            report.push_str(&format!(
                "{name}: {}\n",
                if present { "OK" } else { "MISSING" }
            ));
        }
        report
            .push_str("Playback endpoints (choose CABLE Input; call apps choose CABLE Output):\n");
        for name in av_relay_windows::audio::devices()? {
            report.push_str(&format!("  {name}\n"));
        }
    }
    let path = store.dir().join("diagnostics.txt");
    std::fs::write(&path, &report)?;
    println!("{report}\nSaved to {}", path.display());
    if missing {
        anyhow::bail!("Required GStreamer plugins are missing; see diagnostics.txt");
    }
    Ok(())
}
