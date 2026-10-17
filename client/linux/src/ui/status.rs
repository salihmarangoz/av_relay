//! The status window: everything the tray menu offers, in a window. It opens
//! when AV Relay is launched (at login too, unless it starts minimized), and
//! when there is no tray to show (the desktop's tray support is off).

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::{Device, Status};
use crate::proto::CaptureState;

use super::tray::{FULL_HD, HD};
use super::{Ui, UiEvent, autostart};

pub fn show(ui: &Rc<Ui>) {
    if let Some(win) = ui.status_window.borrow().as_ref().and_then(|w| w.upgrade()) {
        win.present();
        return;
    }
    let win = build(ui);
    *ui.status_window.borrow_mut() = Some(win.downgrade());
    win.present();
}

fn device_subtitle(d: &Device) -> String {
    let here = if d.used_here { "In use on this computer" } else { "Ready" };
    let sender = match (d.state, d.error.as_deref(), d.name.as_deref()) {
        (Some(CaptureState::Error), err, _) => format!(" · sender: {}", err.unwrap_or("error").replace('-', " ")),
        (Some(CaptureState::On), _, Some(name)) => format!(" · {name}"),
        (Some(CaptureState::Starting), _, _) => " · starting…".into(),
        _ => String::new(),
    };
    format!("{here}{sender}")
}

fn build(ui: &Rc<Ui>) -> adw::Window {
    let page = adw::PreferencesPage::new();

    let sender_group = adw::PreferencesGroup::new();
    let sender_row = adw::ActionRow::builder().title("AV Relay").build();
    let state_icon = gtk::Image::from_icon_name(&format!("{}-on-symbolic", super::APP_ID));
    sender_row.add_prefix(&state_icon);
    sender_group.add(&sender_row);
    let camera_row = adw::ActionRow::builder().title("AV Relay Camera").build();
    let mic_row = adw::ActionRow::builder().title("AV Relay Microphone").build();
    sender_group.add(&camera_row);
    sender_group.add(&mic_row);
    page.add(&sender_group);

    if !ui.core.problems().is_empty() {
        let problems = adw::PreferencesGroup::builder().title("Needs attention").build();
        for p in ui.core.problems() {
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(p)).title_lines(0).build();
            row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            row.set_title_selectable(true);
            problems.add(&row);
        }
        page.add(&problems);
    }

    let settings = adw::PreferencesGroup::builder().title("Settings").build();
    let enable = adw::SwitchRow::builder()
        .title("Enable AV Relay")
        .subtitle("Off is as if AV Relay were closed: no connection, no virtual camera or microphone")
        .active(ui.core.store().settings().unwrap_or_default().enabled)
        .build();
    enable.connect_active_notify({
        let ui = ui.clone();
        move |row| {
            ui.enabled.set(row.is_active());
            let _ = ui.events.try_send(UiEvent::SetEnabled(row.is_active()));
        }
    });
    settings.add(&enable);
    let quality = adw::ComboRow::builder()
        .title("Camera quality")
        .model(&gtk::StringList::new(&["1080p, 30 fps", "720p, 30 fps"]))
        .build();
    let current = ui.core.store().settings().unwrap_or_default().video;
    quality.set_selected(if current == HD { 1 } else { 0 });
    quality.connect_selected_notify({
        let events = ui.events.clone();
        move |row| {
            let video = if row.selected() == 1 { HD } else { FULL_HD };
            let _ = events.try_send(UiEvent::SetVideo(video));
        }
    });
    let keep = adw::SwitchRow::builder()
        .title("Keep virtual devices on")
        .subtitle("Keep the camera and microphone (and the default input) while the sender is away")
        .active(ui.core.store().settings().unwrap_or_default().keep_devices)
        .build();
    keep.connect_active_notify({
        let events = ui.events.clone();
        move |row| {
            let _ = events.try_send(UiEvent::SetKeepDevices(row.is_active()));
        }
    });
    let login = adw::SwitchRow::builder().title("Start at login").active(autostart::is_enabled()).build();
    login.connect_active_notify({
        let events = ui.events.clone();
        move |row| {
            let _ = events.try_send(UiEvent::SetAutostart(row.is_active()));
        }
    });
    let minimized = adw::SwitchRow::builder()
        .title("Start minimized")
        .subtitle("At login, only the icon in the top bar; no window")
        .active(ui.core.store().settings().unwrap_or_default().start_minimized)
        .build();
    login.bind_property("active", &minimized, "sensitive").sync_create().build();
    minimized.connect_active_notify({
        let events = ui.events.clone();
        move |row| {
            let _ = events.try_send(UiEvent::SetStartMinimized(row.is_active()));
        }
    });
    settings.add(&quality);
    settings.add(&keep);
    settings.add(&login);
    settings.add(&minimized);
    page.add(&settings);

    let actions = adw::PreferencesGroup::new();
    let pair = adw::ActionRow::builder().title("Pair with a different sender…").activatable(true).build();
    let forget = adw::ActionRow::builder().title("Forget this sender…").activatable(true).build();
    forget.add_css_class("destructive-action");
    let quit = adw::ActionRow::builder().title("Quit AV Relay").activatable(true).build();
    for (row, event) in [(&pair, 0), (&forget, 1), (&quit, 2)] {
        let events = ui.events.clone();
        row.connect_activated(move |_| {
            let _ = events.try_send(match event {
                0 => UiEvent::OpenPair,
                1 => UiEvent::ConfirmForget,
                _ => UiEvent::Quit,
            });
        });
        actions.add(row);
    }
    page.add(&actions);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let win = adw::Window::builder()
        .title("AV Relay")
        .default_width(440)
        .default_height(560)
        .content(&toolbar)
        .application(&ui.app)
        .build();

    let render = {
        let (sender_row, camera_row, mic_row, pair, forget, enable, state_icon) =
            (sender_row.clone(), camera_row.clone(), mic_row.clone(), pair.clone(), forget.clone(), enable.clone(), state_icon.clone());
        let ui = ui.clone();
        move |status: &Status| {
            let (title, subtitle) = match status {
                Status::Starting => ("Starting…".to_string(), String::new()),
                Status::NotPaired { devices } => (
                    "Not paired with a sender".to_string(),
                    super::tray::hold_line(*devices).unwrap_or_else(|| "Pair to use its camera and microphone.".to_string()),
                ),
                Status::Searching { sender, last_error, devices } => (
                    format!("Looking for {sender}…"),
                    [super::tray::hold_line(*devices), last_error.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · "),
                ),
                Status::Connected { sender, .. } => (format!("Connected to {sender}"), String::new()),
                Status::Disabled { sender } => (
                    "AV Relay is off".to_string(),
                    match sender {
                        Some(sender) => format!("Switch it on to use {sender}'s camera and microphone."),
                        None => "No virtual camera or microphone.".to_string(),
                    },
                ),
            };
            let state = match status {
                Status::Disabled { .. } => "disabled",
                Status::Connected { camera, mic, .. } if camera.used_here || mic.used_here => "live",
                Status::Connected { .. } => "on",
                _ => "off",
            };
            state_icon.set_icon_name(Some(&format!("{}-{state}-symbolic", super::APP_ID)));
            // Follows the tray's switch too. From the last choice, not the
            // status: that lags behind while a session winds down, and
            // following it would flip the switch back.
            enable.set_active(ui.enabled.get());
            // Off is as if closed: no pairing either (like the sender).
            pair.set_sensitive(!matches!(status, Status::Disabled { .. }));
            sender_row.set_title(&glib::markup_escape_text(&title));
            sender_row.set_subtitle(&glib::markup_escape_text(&subtitle));
            let connected = matches!(status, Status::Connected { .. });
            camera_row.set_visible(connected);
            mic_row.set_visible(connected);
            if let Status::Connected { camera, mic, .. } = status {
                camera_row.set_subtitle(&glib::markup_escape_text(&device_subtitle(camera)));
                mic_row.set_subtitle(&glib::markup_escape_text(&device_subtitle(mic)));
            }
            let paired = !matches!(status, Status::NotPaired { .. } | Status::Starting | Status::Disabled { sender: None });
            pair.set_title(if paired { "Pair with a different sender…" } else { "Pair with a sender…" });
            forget.set_visible(paired);
        }
    };
    render(&ui.status.borrow());
    let mut status = ui.status.clone();
    let weak = win.downgrade();
    glib::spawn_future_local(async move {
        while status.changed().await.is_ok() {
            if weak.upgrade().is_none() {
                break;
            }
            render(&status.borrow_and_update());
        }
    });
    win
}
