// SPDX-License-Identifier: GPL-3.0-or-later
//! LAN regression: a microphone-only call must not keep the sender camera on.
use anyhow::{Result, bail};
use av_relay_windows::{
    app::{self, Command, Event, Events},
    proto::CaptureState,
    store::Store,
};
use std::{
    process::{Command as Process, Stdio},
    time::{Duration, Instant},
};
fn wait(events: &Events, video: CaptureState) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(500)) {
            match event {
                Event::Capture { video: v, audio: a } => {
                    println!("sender camera {:?}, microphone {:?}", v.state, a.state);
                    if v.state == video && a.state == CaptureState::On {
                        return Ok(());
                    }
                }
                Event::Problem(problem) => eprintln!("{problem}"),
                _ => {}
            }
        }
    }
    bail!("Sender did not reach camera {video:?}, microphone On")
}
fn main() -> Result<()> {
    let (commands, events, worker) = app::start(Store::open_default()?)?;
    let result = (|| -> Result<()> {
        commands.send(Command::Enable(true))?;
        commands.send(Command::Video(true))?;
        commands.send(Command::Audio(true))?;
        wait(&events, CaptureState::Off)?;
        let mut capture = Process::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "dshow",
                "-i",
                "video=OBS Virtual Camera",
                "-t",
                "4",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        let started = wait(&events, CaptureState::On);
        if started.is_err() {
            let _ = capture.kill();
        }
        let status = capture.wait()?;
        started?;
        if !status.success() {
            bail!("Video reader failed: {status}");
        }
        wait(&events, CaptureState::Off)?;
        println!("PASS: mic-only → camera+mic → mic-only, camera stops with reader");
        Ok(())
    })();
    let _ = commands.send(Command::Quit);
    let _ = worker.join();
    result
}
