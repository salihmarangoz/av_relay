//! The tray icon and its menu (StatusNotifierItem, which Ubuntu's dock
//! shows). Everything it does goes to the GTK side as a [`UiEvent`].

use crate::app::{Device, Hold, Status};
use crate::proto::{CaptureState, VideoFormat};

use super::{APP_ID, UiEvent, icons};

pub const FULL_HD: VideoFormat = VideoFormat { width: 1920, height: 1080, fps: 30 };
pub const HD: VideoFormat = VideoFormat { width: 1280, height: 720, fps: 30 };

pub struct Tray {
    pub status: Status,
    pub problems: Vec<String>,
    pub autostart: bool,
    pub keep_devices: bool,
    pub enabled: bool,
    pub video: VideoFormat,
    pub events: async_channel::Sender<UiEvent>,
}

impl Tray {
    fn send(&self, event: UiEvent) {
        let _ = self.events.try_send(event);
    }

    fn mac_name(&self) -> Option<&str> {
        match &self.status {
            Status::Searching { mac, .. } | Status::Connected { mac, .. } | Status::Disabled { mac: Some(mac) } => Some(mac),
            _ => None,
        }
    }

    fn headline(&self) -> String {
        match &self.status {
            Status::Starting => "Starting…".into(),
            Status::NotPaired { .. } => "Not paired with a Mac".into(),
            Status::Searching { mac, .. } => format!("Looking for {mac}…"),
            Status::Connected { mac, .. } => format!("Connected to {mac}"),
            Status::Disabled { .. } => "Off: no virtual camera or microphone".into(),
        }
    }

    fn state(&self) -> &'static str {
        match &self.status {
            Status::Connected { camera, mic, .. } if camera.used_here || mic.used_here => "live",
            Status::Connected { .. } => "on",
            Status::Disabled { .. } => "disabled",
            _ => "off",
        }
    }
}

/// "Camera: in use", "Microphone: ready", "Camera: in use (Mac: no permission)".
fn device_line(what: &str, d: &Device) -> String {
    let mac = match (d.state, d.error.as_deref()) {
        (Some(CaptureState::Error), Some(err)) => format!(" (Mac: {})", err.replace('-', " ")),
        (Some(CaptureState::Error), None) => " (Mac: error)".into(),
        (Some(CaptureState::Starting), _) if d.used_here => " (starting…)".into(),
        _ => String::new(),
    };
    format!("{what}: {}{mac}", if d.used_here { "in use" } else { "ready" })
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "AV Relay".into()
    }

    fn icon_theme_path(&self) -> String {
        icons::theme_path()
    }

    fn icon_name(&self) -> String {
        format!("{APP_ID}-{}-symbolic", self.state())
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "AV Relay".into(), description: self.headline(), ..Default::default() }
    }

    fn watcher_online(&self) {
        self.send(UiEvent::TrayAvailable(true));
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.send(UiEvent::TrayAvailable(false));
        // Keep the service: the host may come back (e.g. GNOME restarted).
        true
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = Vec::new();
        let info = |label: String| StandardItem { label, enabled: false, ..Default::default() }.into();
        items.push(
            CheckmarkItem {
                label: "Enable AV Relay".into(),
                checked: self.enabled,
                activate: Box::new(|t: &mut Self| {
                    t.enabled = !t.enabled;
                    t.send(UiEvent::SetEnabled(t.enabled));
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(info(self.headline()));
        if let Status::Searching { last_error: Some(e), .. } = &self.status {
            items.push(info(shorten(e, 60)));
        }
        if let Status::Searching { devices, .. } | Status::NotPaired { devices } = &self.status {
            if let Some(line) = hold_line(*devices) {
                items.push(info(line));
            }
        }
        if let Status::Connected { camera, mic, .. } = &self.status {
            items.push(info(device_line("Camera", camera)));
            items.push(info(device_line("Microphone", mic)));
        }
        if !self.problems.is_empty() {
            items.push(
                StandardItem {
                    label: "Camera unavailable: details…".into(),
                    icon_name: "dialog-warning-symbolic".into(),
                    activate: Box::new(|t: &mut Self| t.send(UiEvent::OpenStatus)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);

        let paired = !matches!(self.status, Status::NotPaired { .. } | Status::Starting | Status::Disabled { mac: None });
        items.push(
            StandardItem {
                label: if paired { "Pair with another Mac…".into() } else { "Pair with a Mac…".into() },
                activate: Box::new(|t: &mut Self| t.send(UiEvent::OpenPair)),
                ..Default::default()
            }
            .into(),
        );
        if let Some(mac) = self.mac_name() {
            items.push(
                StandardItem {
                    label: format!("Forget {mac}…"),
                    activate: Box::new(|t: &mut Self| t.send(UiEvent::ConfirmForget)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            SubMenu {
                label: "Camera quality".into(),
                submenu: vec![
                    RadioGroup {
                        selected: if self.video == HD { 1 } else { 0 },
                        select: Box::new(|t: &mut Self, i| {
                            let video = if i == 1 { HD } else { FULL_HD };
                            t.video = video;
                            t.send(UiEvent::SetVideo(video));
                        }),
                        options: vec![
                            RadioItem { label: "1080p, 30 fps".into(), ..Default::default() },
                            RadioItem { label: "720p, 30 fps".into(), ..Default::default() },
                        ],
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
        );
        items.push(
            CheckmarkItem {
                label: "Keep virtual devices on".into(),
                checked: self.keep_devices,
                activate: Box::new(|t: &mut Self| {
                    t.keep_devices = !t.keep_devices;
                    t.send(UiEvent::SetKeepDevices(t.keep_devices));
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            CheckmarkItem {
                label: "Start at login".into(),
                checked: self.autostart,
                activate: Box::new(|t: &mut Self| {
                    t.autostart = !t.autostart;
                    t.send(UiEvent::SetAutostart(t.autostart));
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| t.send(UiEvent::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// "Camera and microphone stay until 11:23" while the Mac is away.
pub fn hold_line(hold: Hold) -> Option<String> {
    match hold {
        Hold::None => None,
        Hold::Always => Some("Camera and microphone kept on".into()),
        Hold::Until(t) => {
            let at = gtk::glib::DateTime::from_unix_local(t as i64).ok().and_then(|d| d.format("%H:%M").ok());
            Some(match at {
                Some(at) => format!("Camera and microphone stay until {at}"),
                None => "Camera and microphone stay for a few minutes".into(),
            })
        }
    }
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}
