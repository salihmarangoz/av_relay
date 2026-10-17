mod app;
mod audio;
mod crypto;
mod devtools;
mod media;
mod proto;
mod store;
mod ui;
mod vcam;

use anyhow::{Result, bail};

const USAGE: &str =
    "usage: av-relay [run | vcam-test | vmic-test [SECONDS] [--default] [--drift PPM] | media-test [SECONDS] | answer-test OFFER.sdp | pair HOST[:PORT] [CODE] | session-test [SECONDS]]\n(no command: the tray app)";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        None => ui::run(),
        Some("run") => run_headless(),
        Some("vcam-test") => devtools::vcam_test(),
        Some("vmic-test") => devtools::vmic_test(rest),
        Some("media-test") => devtools::media_test(rest),
        Some("answer-test") => devtools::answer_test(rest),
        Some("pair") => devtools::pair(rest),
        Some("session-test") => devtools::session_test(rest),
        _ => bail!(USAGE),
    }
}

/// Runs the client without a tray: connects to the paired sender and logs its
/// status. Ctrl+C or SIGTERM quits cleanly (the default input is given back).
fn run_headless() -> Result<()> {
    let store = store::Store::open_default()?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        let core = app::Core::new(store)?;
        for p in core.problems() {
            log::warn!("{p}");
        }
        let (status_tx, mut status_rx) = tokio::sync::watch::channel(app::Status::Starting);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let runner = tokio::spawn(app::run(core, status_tx, cmd_rx));
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            tokio::select! {
                changed = status_rx.changed() => {
                    if changed.is_err() { break; }
                    println!("status: {:?}", *status_rx.borrow());
                }
                _ = tokio::signal::ctrl_c() => { let _ = cmd_tx.send(app::Command::Quit); break; }
                _ = term.recv() => { let _ = cmd_tx.send(app::Command::Quit); break; }
            }
        }
        let _ = runner.await;
        anyhow::Ok(())
    })
}
