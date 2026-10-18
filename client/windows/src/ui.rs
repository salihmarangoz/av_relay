// SPDX-License-Identifier: GPL-3.0-or-later
//! Ubuntu's status/preferences and pairing flow, using Win32 controls and GDI.
#![allow(unsafe_op_in_unsafe_fn)]
mod actions;
mod layout;
mod paint;
use crate::{
    app::{Command, Commands, Event, Events},
    autostart,
    icons::{self, State},
    proto::{CaptureState, DeviceState},
    store::{Settings, Store, VIDEO_PRESETS},
    wide,
};
use anyhow::{Result, bail};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    mem::size_of,
    ptr,
    sync::mpsc::TryRecvError,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::*,
        HiDpi::GetDpiForSystem,
        Input::KeyboardAndMouse::{EnableWindow, GetDoubleClickTime, IsWindowEnabled, SetFocus},
        Shell::*,
        WindowsAndMessaging::*,
    },
};
const TRAY: u32 = WM_APP + 1;
const EVENT_TIMER: usize = 1;
/// A single click on the tray icon opens the menu once it can't be a double click.
const MENU_TIMER: usize = 2;
const QUIT_MESSAGE: u32 = WM_APP + 12;
const PAIR: u16 = 101;
const SCAN: u16 = 102;
const VIDEO: u16 = 103;
const AUDIO: u16 = 104;
const FORGET: u16 = 105;
const RESOLUTION: u16 = 106;
const CABLE: u16 = 107;
const NEXT: u16 = 108;
const FOUND: u16 = 110;
const AUTO: u16 = 111;
const MANUAL: u16 = 112;
const BACK: u16 = 113;
const DONE: u16 = 114;
const SHOW: u16 = 201;
const QUIT: u16 = 202;
const ENABLE: u16 = 203;
const OPEN_PAIR: u16 = 204;
const LOGIN: u16 = 205;
const MINIMIZED: u16 = 206;
const RES_MENU: u16 = 300;
const BG: COLORREF = rgb(246, 245, 244);
const INK: COLORREF = rgb(32, 32, 32);
const DIM: COLORREF = rgb(100, 100, 100);
const WHITE: COLORREF = rgb(255, 255, 255);
const BLUE: COLORREF = rgb(53, 132, 228);
const RED: COLORREF = rgb(192, 28, 40);
const fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    r | (g << 8) | (b << 16)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Status,
    Address,
    Code,
    Done,
}
#[derive(Clone, Copy)]
enum Group {
    Status,
    Address,
    Auto,
    Manual,
    Code,
    Done,
}
struct Ui {
    commands: Commands,
    events: Events,
    store: Store,
    settings: Settings,
    hwnd: HWND,
    page: Page,
    manual: bool,
    busy: bool,
    dpi: i32,
    controls: Vec<(HWND, Group)>,
    colours: HashMap<isize, (COLORREF, HBRUSH)>,
    bg: HBRUSH,
    white: HBRUSH,
    font: HFONT,
    small: HFONT,
    bold: HFONT,
    title: HFONT,
    code_font: HFONT,
    host: HWND,
    port: HWND,
    code: HWND,
    list: HWND,
    next: HWND,
    pair_button: HWND,
    pair_action: HWND,
    forget_action: HWND,
    resolution: HWND,
    cable: HWND,
    status_label: HWND,
    detail_label: HWND,
    camera_label: HWND,
    mic_label: HWND,
    footer: HWND,
    address_error: HWND,
    code_error: HWND,
    found_note: HWND,
    done_title: HWND,
    enable_button: HWND,
    video_button: HWND,
    audio_button: HWND,
    login_button: HWND,
    min_button: HWND,
    enabled: bool,
    video_on: bool,
    audio_on: bool,
    connected: bool,
    sender: Option<String>,
    headline: String,
    problem: String,
    video_state: Option<DeviceState>,
    audio_state: Option<DeviceState>,
    found: Vec<crate::discovery::FoundSender>,
    target: Option<(String, u16)>,
    notice: NOTIFYICONDATAW,
    tray_available: bool,
    taskbar_created: u32,
    last_tip: String,
    last_icon: Option<(State, bool)>,
    /// The tray's double click is followed by a button-up: not a click.
    tray_double: bool,
}
impl Ui {
    fn px(&self, n: i32) -> i32 {
        n * self.dpi / 96
    }
    fn send(&self, c: Command) {
        let _ = self.commands.send(c);
    }
    fn state(&self) -> State {
        if !self.enabled {
            State::Disabled
        } else if self.connected {
            let on = |s: &Option<DeviceState>| s.as_ref().is_some_and(|s| s.state == CaptureState::On);
            if on(&self.video_state) || on(&self.audio_state) {
                State::Live
            } else {
                State::On
            }
        } else {
            State::Off
        }
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        unsafe {
            for object in [
                self.bg,
                self.white,
                self.font,
                self.small,
                self.bold,
                self.title,
                self.code_font,
            ] {
                DeleteObject(object);
            }
        }
    }
}
pub fn run(store: &Store, commands: Commands, events: Events, minimized: bool) -> Result<()> {
    unsafe {
        SetProcessDPIAware();
        let dpi = GetDpiForSystem() as i32;
        let font = |size: i32, weight: i32, family: &str| {
            CreateFontW(
                -size * dpi / 96,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                wide(family).as_ptr(),
            )
        };
        let settings = store.settings()?;
        let mut ui = Box::new(RefCell::new(Ui {
            commands,
            events,
            store: store.clone(),
            enabled: settings.enabled,
            settings,
            hwnd: ptr::null_mut(),
            page: Page::Status,
            manual: false,
            busy: false,
            dpi,
            controls: Vec::new(),
            colours: HashMap::new(),
            bg: CreateSolidBrush(BG),
            white: CreateSolidBrush(WHITE),
            font: font(14, 400, "Segoe UI"),
            small: font(12, 400, "Segoe UI"),
            bold: font(15, 600, "Segoe UI"),
            title: font(24, 600, "Segoe UI"),
            code_font: font(30, 400, "Consolas"),
            host: ptr::null_mut(),
            port: ptr::null_mut(),
            code: ptr::null_mut(),
            list: ptr::null_mut(),
            next: ptr::null_mut(),
            pair_button: ptr::null_mut(),
            pair_action: ptr::null_mut(),
            forget_action: ptr::null_mut(),
            resolution: ptr::null_mut(),
            cable: ptr::null_mut(),
            status_label: ptr::null_mut(),
            detail_label: ptr::null_mut(),
            camera_label: ptr::null_mut(),
            mic_label: ptr::null_mut(),
            footer: ptr::null_mut(),
            address_error: ptr::null_mut(),
            code_error: ptr::null_mut(),
            found_note: ptr::null_mut(),
            done_title: ptr::null_mut(),
            enable_button: ptr::null_mut(),
            video_button: ptr::null_mut(),
            audio_button: ptr::null_mut(),
            login_button: ptr::null_mut(),
            min_button: ptr::null_mut(),
            video_on: false,
            audio_on: false,
            connected: false,
            sender: None,
            headline: "Starting…".into(),
            problem: String::new(),
            video_state: None,
            audio_state: None,
            found: Vec::new(),
            target: None,
            notice: std::mem::zeroed(),
            tray_available: false,
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            last_tip: String::new(),
            last_icon: None,
            tray_double: false,
        }));
        let class = wide("AVRelayWindowsReceiver");
        let instance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hIcon: icons::app(32 * dpi / 96),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&wc) == 0 {
            bail!("Registering the settings window failed");
        }
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("AV Relay").as_ptr(),
            // Clip the controls: painting the window mustn't wipe them first.
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            536 * dpi / 96,
            838 * dpi / 96,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            (&mut *ui as *mut RefCell<Ui>).cast(),
        );
        if hwnd.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        ui.borrow_mut().hwnd = hwnd;
        ui.borrow_mut().init();
        if !minimized || !ui.borrow().tray_available {
            ShowWindow(hwnd, SW_SHOW);
        }
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let result = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
}
thread_local! {
    /// A paint or draw message came while the Ui was borrowed and went
    /// unanswered: redraw everything on the next tick.
    static MISSED_PAINT: Cell<bool> = const { Cell::new(false) };
}
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RefCell<Ui>;
    if state.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let cell = &*state;
    // Modal loops (the tray menu, a message box) dispatch this window's
    // paint and draw messages themselves, so they must run without the
    // borrow: otherwise those messages fall through to the defaults below
    // and leave the window half drawn.
    match message {
        TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => {
                    if let Ok(mut ui) = cell.try_borrow_mut()
                        && !std::mem::take(&mut ui.tray_double)
                    {
                        SetTimer(hwnd, MENU_TIMER, GetDoubleClickTime(), None);
                    }
                }
                WM_LBUTTONDBLCLK => {
                    KillTimer(hwnd, MENU_TIMER);
                    if let Ok(mut ui) = cell.try_borrow_mut() {
                        ui.tray_double = true;
                        ui.command(SHOW, 0);
                    }
                }
                WM_RBUTTONUP => {
                    KillTimer(hwnd, MENU_TIMER);
                    tray_menu(hwnd, cell);
                }
                _ => {}
            }
            return 0;
        }
        WM_TIMER if wparam == MENU_TIMER => {
            KillTimer(hwnd, MENU_TIMER);
            tray_menu(hwnd, cell);
            return 0;
        }
        WM_COMMAND if (wparam & 0xffff) as u16 == FORGET => {
            if MessageBoxW(
                hwnd,
                wide("Forget this sender? You will need a new code to pair again.").as_ptr(),
                wide("AV Relay").as_ptr(),
                MB_YESNO | MB_ICONQUESTION,
            ) != IDYES
            {
                return 0;
            }
        }
        _ => {}
    }
    let Ok(mut ui) = cell.try_borrow_mut() else {
        match message {
            WM_PAINT | WM_ERASEBKGND | WM_DRAWITEM | WM_CTLCOLORSTATIC => MISSED_PAINT.set(true),
            // The default would destroy the window: hide it as usual.
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                return 0;
            }
            // Still end the loop, so run() returns and main quits the app.
            WM_DESTROY => PostQuitMessage(0),
            _ => {}
        }
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    if message == ui.taskbar_created {
        ui.tray_available = Shell_NotifyIconW(NIM_ADD, &ui.notice) != 0;
        return 0;
    }
    match message {
        WM_COMMAND => {
            ui.command((wparam & 0xffff) as u16, ((wparam >> 16) & 0xffff) as u16);
            return 0;
        }
        WM_TIMER => {
            ui.tick();
            return 0;
        }
        WM_PAINT => {
            ui.paint();
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_CTLCOLORSTATIC => {
            if let Some(&(colour, brush)) = ui.colours.get(&lparam) {
                let dc = wparam as HDC;
                SetTextColor(dc, colour);
                SetBkMode(dc, TRANSPARENT as i32);
                return brush as isize;
            }
        }
        WM_DRAWITEM => {
            ui.draw_item(&*(lparam as *const DRAWITEMSTRUCT));
            return 1;
        }
        WM_SETTINGCHANGE => {
            ui.update_tray(true);
            return 0;
        }
        QUIT_MESSAGE => {
            ui.command(QUIT, 0);
            return 0;
        }
        WM_CLOSE => {
            ShowWindow(
                hwnd,
                if ui.tray_available {
                    SW_HIDE
                } else {
                    SW_MINIMIZE
                },
            );
            return 0;
        }
        WM_DESTROY => {
            KillTimer(hwnd, EVENT_TIMER);
            KillTimer(hwnd, MENU_TIMER);
            Shell_NotifyIconW(NIM_DELETE, &ui.notice);
            ui.send(Command::Quit);
            PostQuitMessage(0);
            return 0;
        }
        WM_ENDSESSION if wparam != 0 => {
            ui.send(Command::Quit);
            return 0;
        }
        _ => {}
    }
    // Without the borrow: the defaults run modal loops (moving the window,
    // the system menu) and send WM_CLOSE from the title bar's X, all of which
    // come back here and need the Ui.
    drop(ui);
    DefWindowProcW(hwnd, message, wparam, lparam)
}
/// Shows the tray menu and carries out the choice as a menu command.
unsafe fn tray_menu(hwnd: HWND, cell: &RefCell<Ui>) {
    let Ok(ui) = cell.try_borrow() else { return };
    let menu = ui.menu();
    drop(ui);
    let mut point = POINT { x: 0, y: 0 };
    GetCursorPos(&mut point);
    // Without this the menu stays open after a click elsewhere.
    SetForegroundWindow(hwnd);
    let choice = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        0,
        hwnd,
        ptr::null(),
    );
    DestroyMenu(menu);
    PostMessageW(hwnd, WM_NULL, 0, 0);
    if choice > 0 {
        SendMessageW(hwnd, WM_COMMAND, choice as u16 as usize, 0);
    }
}
fn device_subtitle(on: bool, state: Option<&DeviceState>, device: &str) -> String {
    let status = match state.map(|s| s.state) {
        Some(CaptureState::Starting) if on => "Starting…",
        Some(CaptureState::Error) => "Needs attention",
        Some(CaptureState::On) if on => "In use on this computer",
        _ => "Ready",
    };
    format!("{status} · {device}")
}
unsafe fn menu_item(menu: HMENU, id: u16, label: &str, on: bool, enabled: bool) {
    AppendMenuW(
        menu,
        MF_STRING | if on { MF_CHECKED } else { 0 } | if enabled { 0 } else { MF_GRAYED },
        id as usize,
        wide(label).as_ptr(),
    );
}
/// Only on a change: setting a control's text or state repaints it at
/// once, inside the Ui borrow, where it can only get default colours (a
/// flash on every refresh).
unsafe fn set_text(hwnd: HWND, value: &str) {
    if text(hwnd) != value {
        SetWindowTextW(hwnd, wide(value).as_ptr());
    }
}
unsafe fn enable(hwnd: HWND, on: bool) {
    if (IsWindowEnabled(hwnd) != 0) != on {
        EnableWindow(hwnd, on as i32);
    }
}
unsafe fn select(combo: HWND, index: usize) {
    if SendMessageW(combo, CB_GETCURSEL, 0, 0) != index as isize {
        SendMessageW(combo, CB_SETCURSEL, index, 0);
    }
}
unsafe fn text(hwnd: HWND) -> String {
    let n = GetWindowTextLengthW(hwnd);
    let mut out = vec![0u16; n as usize + 1];
    let n = GetWindowTextW(hwnd, out.as_mut_ptr(), out.len() as i32);
    String::from_utf16_lossy(&out[..n as usize])
}
unsafe fn combo_text(hwnd: HWND, index: usize) -> String {
    let n = SendMessageW(hwnd, CB_GETLBTEXTLEN, index, 0);
    if n < 0 {
        return String::new();
    }
    let mut out = vec![0u16; n as usize + 1];
    SendMessageW(hwnd, CB_GETLBTEXT, index, out.as_mut_ptr() as isize);
    String::from_utf16_lossy(&out[..n as usize])
}
