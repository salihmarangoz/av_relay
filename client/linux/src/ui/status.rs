//! The status window: everything the tray menu offers, in a window. It opens
//! when AV Relay is launched while it already runs, and when there is no
//! tray to show (the desktop's tray support is off).

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
    let mac = match (d.state, d.error.as_deref(), d.name.as_deref()) {
        (Some(CaptureState::Error), err, _) => format!(" · Mac: {}", err.unwrap_or("error").replace('-', " ")),
        (Some(CaptureState::On), _, Some(name)) => format!(" · {name}"),
        (Some(CaptureState::Starting), _, _) => " · starting…".into(),
        _ => String::new(),
    };
    format!("{here}{mac}")
}

fn build(ui: &Rc<Ui>) -> adw::Window {
    let page = adw::PreferencesPage::new();

    let mac_group = adw::PreferencesGroup::new();
    let mac_row = adw::ActionRow::builder().title("AV Relay").build();
    mac_row.add_prefix(&gtk::Image::from_icon_name(&format!("{}-on-symbolic", super::APP_ID)));
    mac_group.add(&mac_row);
    let camera_row = adw::ActionRow::builder().title("MacBook Camera").build();
    let mic_row = adw::ActionRow::builder().title("MacBook Microphone").build();
    mac_group.add(&camera_row);
    mac_group.add(&mic_row);
    page.add(&mac_group);

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
    let login = adw::SwitchRow::builder().title("Start at login").active(autostart::is_enabled()).build();
    login.connect_active_notify({
        let events = ui.events.clone();
        move |row| {
            let _ = events.try_send(UiEvent::SetAutostart(row.is_active()));
        }
    });
    settings.add(&quality);
    settings.add(&login);
    page.add(&settings);

    let actions = adw::PreferencesGroup::new();
    let pair = adw::ActionRow::builder().title("Pair with another Mac…").activatable(true).build();
    let forget = adw::ActionRow::builder().title("Forget this Mac…").activatable(true).build();
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
        let (mac_row, camera_row, mic_row, pair, forget) = (mac_row.clone(), camera_row.clone(), mic_row.clone(), pair.clone(), forget.clone());
        move |status: &Status| {
            let (title, subtitle) = match status {
                Status::Starting => ("Starting…".to_string(), String::new()),
                Status::NotPaired => ("Not paired with a Mac".to_string(), "Pair to use its camera and microphone.".to_string()),
                Status::Searching { mac, last_error } => (format!("Looking for {mac}…"), last_error.clone().unwrap_or_default()),
                Status::Connected { mac, .. } => (format!("Connected to {mac}"), String::new()),
            };
            mac_row.set_title(&glib::markup_escape_text(&title));
            mac_row.set_subtitle(&glib::markup_escape_text(&subtitle));
            let connected = matches!(status, Status::Connected { .. });
            camera_row.set_visible(connected);
            mic_row.set_visible(connected);
            if let Status::Connected { camera, mic, .. } = status {
                camera_row.set_subtitle(&glib::markup_escape_text(&device_subtitle(camera)));
                mic_row.set_subtitle(&glib::markup_escape_text(&device_subtitle(mic)));
            }
            let paired = !matches!(status, Status::NotPaired | Status::Starting);
            pair.set_title(if paired { "Pair with another Mac…" } else { "Pair with a Mac…" });
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
