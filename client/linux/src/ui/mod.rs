//! The desktop app: a tray icon (the main UI) and the pairing window, over
//! the core in `app`. GTK owns the main thread; the core, the tray and all
//! networking run on a tokio runtime; they talk through channels.

mod autostart;
mod pair;
mod status;
mod tray;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use anyhow::{Context, Result};
use gtk::{gio, glib};
use ksni::TrayMethods;
use tokio::sync::{mpsc, watch};

use crate::app::{self, Command, Core, Status};
use crate::proto::VideoFormat;
use crate::store::Store;

pub const APP_ID: &str = "io.github.salihmarangoz.avrelay";

/// What the tray (or anything off the GTK thread) asks the GTK side to do.
#[derive(Debug)]
pub enum UiEvent {
    OpenPair,
    ConfirmForget,
    SetVideo(VideoFormat),
    SetAutostart(bool),
    /// The desktop's tray host appeared or went away.
    TrayAvailable(bool),
    Quit,
}

/// State the GTK side shares between windows.
pub struct Ui {
    app: adw::Application,
    rt: Arc<tokio::runtime::Runtime>,
    core: Arc<Core>,
    commands: mpsc::UnboundedSender<Command>,
    status: watch::Receiver<Status>,
    events: async_channel::Sender<UiEvent>,
    tray_available: std::cell::Cell<bool>,
    pair_window: RefCell<Option<glib::WeakRef<adw::Window>>>,
    status_window: RefCell<Option<glib::WeakRef<adw::Window>>>,
}

pub fn run() -> Result<()> {
    let app = adw::Application::builder().application_id(APP_ID).build();
    // One instance: a second start just asks the running one to show itself.
    app.register(None::<&gio::Cancellable>).context("registering the application")?;
    if app.is_remote() {
        // Hands the activation to the running instance and returns.
        app.run_with_args::<&str>(&[]);
        return Ok(());
    }

    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().build()?);
    let store = Store::open_default()?;
    let mut settings = store.settings().unwrap_or_default();
    if !settings.autostart_offered {
        // Start at login unless the user turns it off (the default the user chose).
        if let Err(e) = autostart::set(true) {
            log::warn!("enabling start at login: {e:#}");
        }
        settings.autostart_offered = true;
        let _ = store.save_settings(&settings);
    }
    let core = Core::new(store)?;
    let (status_tx, status_rx) = watch::channel(Status::Starting);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let runner = rt.spawn(app::run(core.clone(), status_tx, cmd_rx));

    let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
    let tray = tray::Tray {
        status: Status::Starting,
        problems: core.problems().to_vec(),
        autostart: autostart::is_enabled(),
        video: settings.video,
        events: ui_tx.clone(),
    };
    // Don't fail without a tray host: at login GNOME's may come up after us,
    // and some desktops have it off. The status window stands in meanwhile.
    let tray = rt.block_on(tray.assume_sni_available(true).spawn()).context("starting the tray icon")?;
    rt.spawn({
        let tray = tray.clone();
        let mut status = status_rx.clone();
        async move {
            loop {
                let now = status.borrow_and_update().clone();
                tray.update(|t| t.status = now).await;
                if status.changed().await.is_err() {
                    break;
                }
            }
        }
    });

    let ui = Rc::new(Ui {
        app: app.clone(),
        rt: rt.clone(),
        core,
        commands: cmd_tx.clone(),
        status: status_rx,
        events: ui_tx.clone(),
        tray_available: std::cell::Cell::new(true),
        pair_window: RefCell::default(),
        status_window: RefCell::default(),
    });
    // A tray app has no main window; keep running without one.
    let hold = app.hold();
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        for dir in icons::dev_dirs() {
            theme.add_search_path(dir);
        }
    }
    gtk::Window::set_default_icon_name(APP_ID);

    app.connect_activate({
        let ui = ui.clone();
        move |_| {
            // Launched from the app grid (or again while running): show
            // something. Not paired: the pairing window; else the status.
            if ui.core.store().paired_mac().ok().flatten().is_none() {
                pair::show(&ui);
            } else {
                status::show(&ui);
            }
        }
    });

    glib::spawn_future_local({
        let ui = ui.clone();
        let tray = tray.clone();
        async move {
            while let Ok(event) = ui_rx.recv().await {
                match event {
                    UiEvent::OpenPair => pair::show(&ui),
                    UiEvent::ConfirmForget => confirm_forget(&ui),
                    UiEvent::SetVideo(video) => {
                        let store = ui.core.store();
                        let mut s = store.settings().unwrap_or_default();
                        if s.video != video {
                            s.video = video;
                            if let Err(e) = store.save_settings(&s) {
                                log::error!("saving settings: {e:#}");
                            }
                            // The camera's format is fixed while apps may
                            // use it; apply it with a fresh connection.
                            let _ = ui.commands.send(Command::Paired);
                        }
                        ui.rt.spawn({
                            let tray = tray.clone();
                            async move { tray.update(|t| t.video = video).await }
                        });
                    }
                    UiEvent::TrayAvailable(available) => {
                        let was = ui.tray_available.replace(available);
                        if was && !available {
                            log::warn!("no tray host (on GNOME: the Ubuntu AppIndicators extension); showing the status window");
                            status::show(&ui);
                        }
                    }
                    UiEvent::SetAutostart(on) => {
                        if let Err(e) = autostart::set(on) {
                            log::error!("start at login: {e:#}");
                        }
                        let actual = autostart::is_enabled();
                        ui.rt.spawn({
                            let tray = tray.clone();
                            async move { tray.update(|t| t.autostart = actual).await }
                        });
                    }
                    UiEvent::Quit => {
                        ui.app.quit();
                        break;
                    }
                }
            }
        }
    });

    // Quit cleanly on SIGTERM/SIGINT too (logout, package removal, Ctrl+C),
    // so the default input is given back.
    rt.spawn({
        let ui_tx = ui_tx.clone();
        async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else { return };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            let _ = ui_tx.send(UiEvent::Quit).await;
        }
    });

    app.run_with_args::<&str>(&[]);
    drop(hold);
    let _ = cmd_tx.send(Command::Quit);
    rt.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(5), runner).await;
        tray.shutdown().await;
    });
    Ok(())
}

fn confirm_forget(ui: &Rc<Ui>) {
    let mac = match &*ui.status.borrow() {
        Status::Searching { mac, .. } | Status::Connected { mac, .. } => mac.clone(),
        _ => return,
    };
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Forget {mac}?"))
        .body("The camera and microphone go away, and the Mac forgets this computer too if it's connected. To use them again, pair again.")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("forget", "Forget")]);
    dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    let commands = ui.commands.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "forget" {
            let _ = commands.send(Command::Forget);
        }
    });
    // Over the status or pairing window if one is open; on its own otherwise.
    let parent = [&ui.status_window, &ui.pair_window].into_iter().find_map(|w| w.borrow().as_ref().and_then(|w| w.upgrade()));
    dialog.present(parent.as_ref().map(|w| w.upcast_ref::<gtk::Widget>()));
}

pub mod icons {
    use std::path::{Path, PathBuf};

    use super::APP_ID;

    /// Installed, the icons are in the system's hicolor theme. Run from a
    /// source tree, the generated assets hold a hicolor theme of their own;
    /// its parent goes into the icon search path. (As loose files, GTK and
    /// the tray host wouldn't recolour the symbolic icons.)
    pub fn dev_dirs() -> Vec<PathBuf> {
        let installed = format!("/usr/share/icons/hicolor/symbolic/apps/{APP_ID}-on-symbolic.svg");
        if Path::new(&installed).exists() {
            return Vec::new();
        }
        let exe = std::env::current_exe().unwrap_or_default();
        exe.ancestors()
            .map(|d| d.join("assets/generated/linux"))
            .find(|p| p.join("hicolor/index.theme").is_file())
            .into_iter()
            .collect()
    }

    /// The tray's extra icon path (empty when installed).
    pub fn theme_path() -> String {
        dev_dirs().first().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    }
}
