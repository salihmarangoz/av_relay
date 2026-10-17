use super::*;
impl Ui {
    unsafe fn widget(
        &mut self,
        group: Group,
        class: &str,
        title: &str,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        id: u16,
        font: HFONT,
    ) -> HWND {
        let tab = if class == "STATIC" { 0 } else { WS_TABSTOP };
        let hwnd = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(title).as_ptr(),
            WS_CHILD | tab | style,
            self.px(x),
            self.px(y),
            self.px(w),
            self.px(h),
            self.hwnd,
            id as usize as HMENU,
            GetModuleHandleW(ptr::null()),
            ptr::null(),
        );
        SendMessageW(hwnd, WM_SETFONT, font as usize, 0);
        SetWindowTheme(hwnd, wide("Explorer").as_ptr(), ptr::null());
        self.controls.push((hwnd, group));
        hwnd
    }
    unsafe fn label(
        &mut self,
        g: Group,
        text: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        dim: bool,
        card: bool,
        font: HFONT,
    ) -> HWND {
        let c = self.widget(g, "STATIC", text, 0, x, y, w, h, 0, font);
        self.colours.insert(
            c as isize,
            (
                if dim { DIM } else { INK },
                if card { self.white } else { self.bg },
            ),
        );
        c
    }
    unsafe fn button(
        &mut self,
        g: Group,
        text: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        id: u16,
    ) -> HWND {
        self.widget(
            g,
            "BUTTON",
            text,
            BS_PUSHBUTTON as u32,
            x,
            y,
            w,
            h,
            id,
            self.font,
        )
    }
    unsafe fn toggle(&mut self, id: u16, y: i32) -> HWND {
        let name = match id {
            VIDEO => "Camera",
            AUDIO => "Microphone",
            ENABLE => "Enable AV Relay",
            LOGIN => "Start at login",
            _ => "Start minimized",
        };
        self.widget(
            Group::Status,
            "BUTTON",
            name,
            BS_OWNERDRAW as u32,
            438,
            y,
            42,
            26,
            id,
            self.font,
        )
    }
    pub(super) unsafe fn init(&mut self) {
        let g = Group::Status;
        self.label(g, "AV Relay", 82, 24, 360, 30, false, false, self.title);
        self.label(g, "Receiver", 82, 54, 360, 20, true, false, self.small);
        self.status_label = self.label(g, "Starting…", 72, 96, 408, 26, false, true, self.bold);
        self.detail_label = self.label(g, "", 72, 123, 408, 20, true, true, self.small);
        self.label(
            g,
            "AV Relay Camera",
            36,
            162,
            376,
            22,
            false,
            true,
            self.font,
        );
        self.camera_label = self.label(
            g,
            "Ready · OBS Virtual Camera",
            36,
            184,
            376,
            18,
            true,
            true,
            self.small,
        );
        self.video_button = self.toggle(VIDEO, 168);
        self.label(
            g,
            "AV Relay Microphone",
            36,
            214,
            376,
            22,
            false,
            true,
            self.font,
        );
        self.mic_label = self.label(
            g,
            "Ready · CABLE Output",
            36,
            236,
            376,
            18,
            true,
            true,
            self.small,
        );
        self.audio_button = self.toggle(AUDIO, 220);
        self.label(g, "Settings", 24, 282, 440, 22, false, false, self.bold);
        self.label(
            g,
            "Enable AV Relay",
            36,
            326,
            376,
            22,
            false,
            true,
            self.font,
        );
        self.label(
            g,
            "Connect to your paired sender",
            36,
            348,
            376,
            18,
            true,
            true,
            self.small,
        );
        self.enable_button = self.toggle(ENABLE, 330);
        self.label(g, "Resolution", 36, 388, 254, 22, false, true, self.font);
        self.resolution = self.widget(
            g,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            322,
            382,
            158,
            160,
            RESOLUTION,
            self.font,
        );
        for (name, _) in VIDEO_PRESETS {
            SendMessageW(
                self.resolution,
                CB_ADDSTRING,
                0,
                wide(name).as_ptr() as isize,
            );
        }
        self.label(
            g,
            "Start at login",
            36,
            442,
            376,
            22,
            false,
            true,
            self.font,
        );
        self.login_button = self.toggle(LOGIN, 436);
        self.label(
            g,
            "Start minimized",
            36,
            495,
            376,
            22,
            false,
            true,
            self.font,
        );
        self.min_button = self.toggle(MINIMIZED, 490);
        self.label(
            g,
            "Microphone output",
            36,
            556,
            158,
            22,
            false,
            true,
            self.small,
        );
        self.cable = self.widget(
            g,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            198,
            550,
            282,
            200,
            CABLE,
            self.small,
        );
        SendMessageW(
            self.cable,
            CB_ADDSTRING,
            0,
            wide("Automatic: CABLE Input").as_ptr() as isize,
        );
        if let Ok(devices) = crate::audio::devices() {
            for name in devices {
                SendMessageW(self.cable, CB_ADDSTRING, 0, wide(&name).as_ptr() as isize);
            }
        }
        self.pair_action = self.widget(
            g,
            "BUTTON",
            "Pair with a sender…",
            BS_OWNERDRAW as u32,
            22,
            610,
            476,
            40,
            OPEN_PAIR,
            self.font,
        );
        self.forget_action = self.widget(
            g,
            "BUTTON",
            "Forget this sender…",
            BS_OWNERDRAW as u32,
            22,
            650,
            476,
            40,
            FORGET,
            self.font,
        );
        self.widget(
            g,
            "BUTTON",
            "Quit AV Relay",
            BS_OWNERDRAW as u32,
            22,
            690,
            476,
            40,
            QUIT,
            self.font,
        );
        self.footer = self.label(g, "", 24, 750, 472, 42, true, false, self.small);
        self.button(Group::Address, "‹", 20, 20, 40, 30, BACK);
        self.label(
            Group::Address,
            "Pair with a sender",
            72,
            23,
            400,
            30,
            false,
            false,
            self.bold,
        );
        self.label(
            Group::Address,
            "On the sender, choose Allow Pairing… in AV Relay's menu.",
            36,
            98,
            448,
            42,
            false,
            false,
            self.font,
        );
        self.label(
            Group::Address,
            "Select the computer whose camera and microphone you want to use.",
            36,
            144,
            448,
            44,
            true,
            false,
            self.small,
        );
        self.widget(
            Group::Address,
            "BUTTON",
            "Auto-discovery",
            BS_AUTORADIOBUTTON as u32 | BS_PUSHLIKE as u32 | WS_GROUP,
            104,
            206,
            156,
            34,
            AUTO,
            self.font,
        );
        self.widget(
            Group::Address,
            "BUTTON",
            "Manual",
            BS_AUTORADIOBUTTON as u32 | BS_PUSHLIKE as u32,
            260,
            206,
            156,
            34,
            MANUAL,
            self.font,
        );
        self.found_note = self.label(
            Group::Auto,
            "Senders on this network",
            36,
            272,
            280,
            24,
            false,
            false,
            self.bold,
        );
        self.button(Group::Auto, "Search again", 352, 266, 132, 30, SCAN);
        self.list = self.widget(
            Group::Auto,
            "LISTBOX",
            "",
            WS_BORDER | WS_VSCROLL | LBS_NOTIFY as u32,
            36,
            308,
            448,
            128,
            FOUND,
            self.font,
        );
        self.label(
            Group::Manual,
            "Sender address",
            36,
            282,
            310,
            22,
            true,
            false,
            self.small,
        );
        self.host = self.widget(
            Group::Manual,
            "EDIT",
            "",
            WS_BORDER | ES_AUTOHSCROLL as u32,
            36,
            308,
            328,
            32,
            0,
            self.font,
        );
        self.label(
            Group::Manual,
            "Port",
            382,
            282,
            100,
            22,
            true,
            false,
            self.small,
        );
        self.port = self.widget(
            Group::Manual,
            "EDIT",
            "48100",
            WS_BORDER | ES_NUMBER as u32,
            382,
            308,
            102,
            32,
            0,
            self.font,
        );
        self.label(
            Group::Manual,
            "Use the address and port shown in the sender's pairing window.",
            36,
            360,
            448,
            44,
            true,
            false,
            self.small,
        );
        self.next = self.button(Group::Address, "Continue", 178, 466, 164, 38, NEXT);
        self.address_error = self.label(
            Group::Address,
            "",
            36,
            524,
            448,
            54,
            false,
            false,
            self.small,
        );
        self.colours
            .insert(self.address_error as isize, (RED, self.bg));
        self.label(
            Group::Code,
            "Enter the code",
            36,
            132,
            448,
            40,
            false,
            false,
            self.title,
        );
        self.label(Group::Code,"Type the 6-digit code the sender shows. Each code works once and only while its pairing window is open.",36,192,448,60,true,false,self.font);
        self.code = self.widget(
            Group::Code,
            "EDIT",
            "",
            WS_BORDER | ES_CENTER as u32 | ES_AUTOHSCROLL as u32,
            164,
            280,
            192,
            52,
            0,
            self.code_font,
        );
        SendMessageW(self.code, EM_SETLIMITTEXT, 7, 0);
        SendMessageW(
            self.code,
            EM_SETCUEBANNER,
            0,
            wide("000 000").as_ptr() as isize,
        );
        self.button(Group::Code, "Back", 94, 374, 150, 38, BACK);
        self.pair_button = self.button(Group::Code, "Pair", 276, 374, 150, 38, PAIR);
        self.code_error = self.label(Group::Code, "", 36, 448, 448, 76, false, false, self.small);
        self.colours
            .insert(self.code_error as isize, (RED, self.bg));
        self.done_title = self.label(
            Group::Done,
            "Paired",
            36,
            210,
            448,
            50,
            false,
            false,
            self.title,
        );
        self.label(Group::Done,"Choose OBS Virtual Camera and CABLE Output in your apps. Start and stop capture using AV Relay's switches.",36,280,448,78,true,false,self.font);
        self.button(Group::Done, "Done", 178, 400, 164, 38, DONE);
        if let Ok(Some(sender)) = self.store.paired() {
            set_text(self.host, &sender.address);
            set_text(self.port, &sender.port.to_string());
            self.sender = Some(sender.name);
        }
        self.notice.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        self.notice.hWnd = self.hwnd;
        self.notice.uID = 1;
        self.notice.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        self.notice.uCallbackMessage = TRAY;
        self.update_tray(true);
        self.tray_available = Shell_NotifyIconW(NIM_ADD, &self.notice) != 0;
        SendMessageW(
            self.hwnd,
            WM_SETICON,
            ICON_BIG as usize,
            icons::app(self.px(48)) as isize,
        );
        SendMessageW(
            self.hwnd,
            WM_SETICON,
            ICON_SMALL as usize,
            icons::app(GetSystemMetrics(SM_CXSMICON)) as isize,
        );
        SetTimer(self.hwnd, 1, 200, None);
        self.show_page(if self.sender.is_some() {
            Page::Status
        } else {
            Page::Address
        });
        if self.page == Page::Address {
            self.search();
        }
        self.refresh();
    }
    pub(super) unsafe fn show_page(&mut self, page: Page) {
        self.page = page;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: self.px(520),
            bottom: self.px(if page == Page::Status { 800 } else { 600 }),
        };
        AdjustWindowRect(
            &mut rect,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
        );
        SetWindowPos(
            self.hwnd,
            ptr::null_mut(),
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER,
        );
        SetWindowTextW(
            self.hwnd,
            wide(if page == Page::Status {
                "AV Relay"
            } else {
                "Pair with a sender"
            })
            .as_ptr(),
        );
        for &(hwnd, group) in &self.controls {
            let show = match group {
                Group::Status => page == Page::Status,
                Group::Address => page == Page::Address,
                Group::Auto => page == Page::Address && !self.manual,
                Group::Manual => page == Page::Address && self.manual,
                Group::Code => page == Page::Code,
                Group::Done => page == Page::Done,
            };
            ShowWindow(hwnd, if show { SW_SHOW } else { SW_HIDE });
        }
        SendMessageW(
            GetDlgItem(self.hwnd, AUTO as i32),
            BM_SETCHECK,
            if !self.manual {
                BST_CHECKED as usize
            } else {
                0
            },
            0,
        );
        SendMessageW(
            GetDlgItem(self.hwnd, MANUAL as i32),
            BM_SETCHECK,
            if self.manual { BST_CHECKED as usize } else { 0 },
            0,
        );
        InvalidateRect(self.hwnd, ptr::null(), 1);
        if page == Page::Code {
            SetFocus(self.code);
        }
    }
}
