// SPDX-License-Identifier: GPL-3.0-or-later
use crate::wide;
use std::{cell::RefCell, collections::HashMap, ptr};
use windows_sys::Win32::{
    System::{LibraryLoader::GetModuleHandleW, Registry::*},
    UI::WindowsAndMessaging::*,
};
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Off,
    On,
    Live,
    Disabled,
}
impl State {
    fn index(self) -> u16 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Live => 2,
            Self::Disabled => 3,
        }
    }
}
pub fn app(size: i32) -> HICON {
    load(1, size)
}
pub fn glyph(state: State, light_background: bool, size: i32) -> HICON {
    load(
        101 + state.index() * 2 + if light_background { 0 } else { 1 },
        size,
    )
}
/// Loaded once per id and size, and kept. LR_SHARED would hand every size
/// the first one loaded, scaled.
fn load(id: u16, size: i32) -> HICON {
    thread_local! {
        static LOADED: RefCell<HashMap<(u16, i32), HICON>> = RefCell::new(HashMap::new());
    }
    LOADED.with(|loaded| {
        *loaded.borrow_mut().entry((id, size)).or_insert_with(|| unsafe {
            let icon = LoadImageW(
                GetModuleHandleW(ptr::null()),
                id as usize as *const u16,
                IMAGE_ICON,
                size,
                size,
                0,
            );
            if icon.is_null() {
                LoadIconW(ptr::null_mut(), IDI_APPLICATION)
            } else {
                icon
            }
        })
    })
}
pub fn light_taskbar() -> bool {
    let mut value = 0u32;
    let mut size = 4;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize").as_ptr(),
            wide("SystemUsesLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        );
    }
    value != 0
}
