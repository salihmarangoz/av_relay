use super::*;
impl Ui {
    pub(super) unsafe fn search(&mut self) {
        self.found.clear();
        SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
        set_text(self.found_note, "Looking for senders…");
        self.send(Command::Discover);
        self.refresh();
    }
    pub(super) unsafe fn command(&mut self, id: u16, notification: u16) {
        // Controls notify more than their action: a list opening or
        // closing, a double-click. Act on the action alone; the refresh after
        // any other notification put a combo box back to the saved choice
        // before its CBN_SELCHANGE came (the resolution never changed), and a
        // double-click toggled a switch twice. Menu items arrive as clicks.
        let action = match id {
            RESOLUTION | CABLE => notification == CBN_SELCHANGE as u16,
            FOUND => notification == LBN_SELCHANGE as u16,
            _ => notification == BN_CLICKED as u16,
        };
        if !action {
            return;
        }
        match id {
            // Flipped here at once, so a second click before the receiver's
            // answer toggles back; the answer (Event::Demand) corrects it.
            VIDEO => {
                self.video_on = !self.video_on;
                self.send(Command::Video(self.video_on));
            }
            AUDIO => {
                self.audio_on = !self.audio_on;
                self.send(Command::Audio(self.audio_on));
            }
            ENABLE => {
                self.enabled = !self.enabled;
                self.send(Command::Enable(self.enabled));
            }
            LOGIN => {
                if let Err(e) = autostart::set(!autostart::enabled()) {
                    self.problem = format!("Start at login: {e:#}");
                }
            }
            MINIMIZED => {
                self.settings.start_minimized = !self.settings.start_minimized;
                self.send(Command::StartMinimized(self.settings.start_minimized));
            }
            RESOLUTION if notification == CBN_SELCHANGE as u16 => {
                self.select_resolution(SendMessageW(self.resolution, CB_GETCURSEL, 0, 0) as usize);
            }
            CABLE if notification == CBN_SELCHANGE as u16 => {
                let index = SendMessageW(self.cable, CB_GETCURSEL, 0, 0);
                if index >= 0 {
                    self.settings.audio_device = if index == 0 {
                        None
                    } else {
                        Some(combo_text(self.cable, index as usize))
                    };
                    self.send(Command::AudioDevice(self.settings.audio_device.clone()));
                }
            }
            OPEN_PAIR => {
                self.busy = false;
                set_text(self.address_error, "");
                self.show_page(Page::Address);
                self.search();
                ShowWindow(self.hwnd, SW_RESTORE);
                SetForegroundWindow(self.hwnd);
            }
            SCAN => self.search(),
            AUTO | MANUAL => {
                self.manual = id == MANUAL;
                self.show_page(Page::Address);
                if self.manual {
                    SetFocus(self.host);
                }
            }
            FOUND => {}
            NEXT => {
                let target = if self.manual {
                    let host = text(self.host).trim().to_owned();
                    match text(self.port).parse::<u16>() {
                        Ok(port) if port > 0 && !host.is_empty() => Some((host, port)),
                        _ => None,
                    }
                } else {
                    let i = SendMessageW(self.list, LB_GETCURSEL, 0, 0);
                    self.found
                        .get(i as usize)
                        .map(|s| (s.address.to_string(), s.port))
                };
                if let Some(target) = target {
                    self.target = Some(target);
                    set_text(self.code, "");
                    set_text(self.code_error, "");
                    self.show_page(Page::Code);
                } else {
                    set_text(
                        self.address_error,
                        "Choose a sender, or enter its address and port.",
                    );
                }
            }
            PAIR => {
                let code: String = text(self.code)
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                if code.len() != 6 || !code.bytes().all(|c| c.is_ascii_digit()) {
                    set_text(self.code_error, "Enter the six digits shown by the sender.");
                } else if !self.busy {
                    if let Some((host, port)) = self.target.clone() {
                        self.busy = true;
                        EnableWindow(self.pair_button, 0);
                        set_text(self.code_error, "Pairing…");
                        self.send(Command::Pair { host, port, code });
                        set_text(self.code, "");
                    }
                }
            }
            BACK => {
                if self.page == Page::Code {
                    self.show_page(Page::Address);
                } else if self.sender.is_some() {
                    self.show_page(Page::Status);
                } else {
                    ShowWindow(self.hwnd, SW_HIDE);
                }
            }
            DONE => self.show_page(Page::Status),
            // Confirmed by window_proc, outside the borrow.
            FORGET => {
                self.sender = None;
                self.connected = false;
                self.send(Command::Forget);
            }
            SHOW => {
                self.show_page(if self.sender.is_some() {
                    Page::Status
                } else {
                    Page::Address
                });
                ShowWindow(self.hwnd, SW_RESTORE);
                SetForegroundWindow(self.hwnd);
            }
            QUIT => {
                self.send(Command::Quit);
                KillTimer(self.hwnd, EVENT_TIMER);
                KillTimer(self.hwnd, MENU_TIMER);
                Shell_NotifyIconW(NIM_DELETE, &self.notice);
                DestroyWindow(self.hwnd);
                PostQuitMessage(0);
                return;
            }
            n if n >= RES_MENU && (n - RES_MENU) < VIDEO_PRESETS.len() as u16 => {
                self.select_resolution((n - RES_MENU) as usize)
            }
            _ => {}
        }
        self.refresh();
    }
    unsafe fn select_resolution(&mut self, index: usize) {
        if let Some((_, format)) = VIDEO_PRESETS.get(index) {
            if self.settings.video != *format {
                self.settings.video = *format;
                self.send(Command::Resolution(*format));
            }
        }
    }
    pub(super) unsafe fn tick(&mut self) {
        if MISSED_PAINT.take() {
            RedrawWindow(
                self.hwnd,
                ptr::null(),
                ptr::null_mut(),
                RDW_INVALIDATE | RDW_ALLCHILDREN,
            );
        }
        let mut changed = false;
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    changed = true;
                    match event {
                        Event::Status(s) => self.headline = s,
                        Event::Problem(s) => {
                            self.problem = s;
                            if self.busy {
                                self.busy = false;
                                EnableWindow(self.pair_button, 1);
                                set_text(self.code_error, &self.problem);
                            }
                        }
                        Event::Paired(name) => {
                            self.busy = false;
                            self.sender = Some(name.clone());
                            self.problem.clear();
                            set_text(self.done_title, &format!("Paired with {name}"));
                            EnableWindow(self.pair_button, 1);
                            self.show_page(Page::Done);
                        }
                        Event::Demand { video, audio } => {
                            self.video_on = video;
                            self.audio_on = audio;
                        }
                        Event::Enabled(on) => self.enabled = on,
                        Event::Connection { connected, sender } => {
                            self.connected = connected;
                            self.sender = sender;
                            if connected {
                                self.problem.clear();
                            }
                        }
                        Event::Capture { video, audio } => {
                            self.video_state = Some(video);
                            self.audio_state = Some(audio);
                        }
                        Event::Settings(s) => self.settings = s,
                        Event::Found(found) => {
                            self.found = found;
                            SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
                            for sender in &self.found {
                                SendMessageW(
                                    self.list,
                                    LB_ADDSTRING,
                                    0,
                                    wide(&format!("{}   ·   {}", sender.name, sender.address))
                                        .as_ptr() as isize,
                                );
                            }
                            set_text(
                                self.found_note,
                                if self.found.is_empty() {
                                    "No senders found"
                                } else {
                                    "Senders on this network"
                                },
                            );
                            if self.found.len() == 1 {
                                SendMessageW(self.list, LB_SETCURSEL, 0, 0);
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.problem != "Receiver stopped. Quit and reopen AV Relay." {
                        self.problem = "Receiver stopped. Quit and reopen AV Relay.".into();
                        changed = true;
                    }
                    break;
                }
            }
        }
        if changed {
            self.refresh();
        }
    }
    pub(super) unsafe fn refresh(&mut self) {
        let status = if !self.enabled {
            "AV Relay is off".into()
        } else if self.connected {
            format!(
                "Connected to {}",
                self.sender.as_deref().unwrap_or("sender")
            )
        } else if let Some(sender) = &self.sender {
            format!("Looking for {sender}…")
        } else {
            "No sender paired".into()
        };
        set_text(self.status_label, &status);
        set_text(
            self.detail_label,
            if !self.enabled {
                "Enable AV Relay to connect"
            } else if self.connected {
                "Your sender is ready"
            } else if self.sender.is_some() {
                "Reconnects automatically when available"
            } else {
                "Pair with a sender to get started"
            },
        );
        set_text(
            self.camera_label,
            &device_subtitle(
                self.video_on,
                self.video_state.as_ref(),
                "OBS Virtual Camera",
            ),
        );
        set_text(
            self.mic_label,
            &device_subtitle(self.audio_on, self.audio_state.as_ref(), "CABLE Output"),
        );
        set_text(
            self.pair_action,
            if self.sender.is_some() {
                "Pair with a different sender…"
            } else {
                "Pair with a sender…"
            },
        );
        enable(self.forget_action, self.sender.is_some());
        enable(self.video_button, self.enabled && self.sender.is_some());
        enable(self.audio_button, self.enabled && self.sender.is_some());
        enable(self.min_button, autostart::enabled());
        let selection = VIDEO_PRESETS
            .iter()
            .position(|(_, v)| *v == self.settings.video)
            .unwrap_or(1);
        select(self.resolution, selection);
        let count = SendMessageW(self.cable, CB_GETCOUNT, 0, 0);
        let mut chosen = 0;
        for i in 1..count {
            if self.settings.audio_device.as_deref()
                == Some(combo_text(self.cable, i as usize).as_str())
            {
                chosen = i;
                break;
            }
        }
        select(self.cable, chosen as usize);
        set_text(
            self.footer,
            if self.problem.is_empty() {
                "Choose OBS Virtual Camera and CABLE Output in your apps. The sender's camera and microphone run only while an app uses them."
            } else {
                &self.problem
            },
        );
        self.colours.insert(
            self.footer as isize,
            (if self.problem.is_empty() { DIM } else { RED }, self.bg),
        );
        enable(self.next, self.manual || SendMessageW(self.list, LB_GETCURSEL, 0, 0) >= 0);
        self.update_tray(false);
        InvalidateRect(self.hwnd, ptr::null(), 0);
        // Owner-drawn controls show state set_text doesn't change; the
        // others repaint themselves when their text does.
        for &(hwnd, _) in &self.controls {
            if GetWindowLongW(hwnd, GWL_STYLE) as u32 & BS_TYPEMASK as u32 == BS_OWNERDRAW as u32 {
                InvalidateRect(hwnd, ptr::null(), 0);
            }
        }
    }
    pub(super) unsafe fn update_tray(&mut self, force: bool) {
        let light = icons::light_taskbar();
        let state = self.state();
        let tip = format!(
            "AV Relay · {}",
            if !self.enabled {
                "Disabled".to_string()
            } else if self.connected {
                format!(
                    "Connected to {}",
                    self.sender.as_deref().unwrap_or("sender")
                )
            } else {
                "Waiting for sender".into()
            }
        );
        if force || self.last_icon != Some((state, light)) || self.last_tip != tip {
            self.notice.hIcon = icons::glyph(state, light, GetSystemMetrics(SM_CXSMICON));
            self.notice.szTip.fill(0);
            for (i, c) in tip.encode_utf16().take(126).enumerate() {
                self.notice.szTip[i] = c;
            }
            if self.tray_available {
                Shell_NotifyIconW(NIM_MODIFY, &self.notice);
            }
            self.last_icon = Some((state, light));
            self.last_tip = tip;
        }
    }
    /// The tray menu; tray_menu shows it.
    pub(super) unsafe fn menu(&self) -> HMENU {
        let menu = CreatePopupMenu();
        menu_item(menu, ENABLE, "Enable AV Relay", self.enabled, true);
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        menu_item(
            menu,
            0,
            &if self.connected {
                format!(
                    "Connected to {}",
                    self.sender.as_deref().unwrap_or("sender")
                )
            } else {
                self.headline.clone()
            },
            false,
            false,
        );
        menu_item(
            menu,
            VIDEO,
            "Camera",
            self.video_on,
            self.enabled && self.sender.is_some(),
        );
        menu_item(
            menu,
            AUDIO,
            "Microphone",
            self.audio_on,
            self.enabled && self.sender.is_some(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        menu_item(
            menu,
            OPEN_PAIR,
            if self.sender.is_some() {
                "Pair with a different sender…"
            } else {
                "Pair with a sender…"
            },
            false,
            true,
        );
        menu_item(
            menu,
            FORGET,
            "Forget this sender…",
            false,
            self.sender.is_some(),
        );
        let quality = CreatePopupMenu();
        for (i, (name, _)) in VIDEO_PRESETS.iter().enumerate() {
            menu_item(
                quality,
                RES_MENU + i as u16,
                &format!("{name}, 30 fps"),
                VIDEO_PRESETS[i].1 == self.settings.video,
                true,
            );
        }
        AppendMenuW(
            menu,
            MF_POPUP,
            quality as usize,
            wide("Resolution").as_ptr(),
        );
        menu_item(menu, LOGIN, "Start at login", autostart::enabled(), true);
        menu_item(
            menu,
            MINIMIZED,
            "Start minimized",
            self.settings.start_minimized,
            autostart::enabled(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        menu_item(menu, SHOW, "Open AV Relay", false, true);
        menu_item(menu, QUIT, "Quit AV Relay", false, true);
        menu
    }
}
