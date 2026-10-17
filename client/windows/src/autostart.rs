// SPDX-License-Identifier: GPL-3.0-or-later
use crate::wide;
use anyhow::{Context, Result, bail};
use std::{path::PathBuf, ptr};
use windows_sys::Win32::System::Registry::*;
const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
pub fn enabled() -> bool {
    let mut bytes = 0;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(KEY).as_ptr(),
            wide("AV Relay").as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut bytes,
        ) == 0
            && bytes > 2
    }
}
pub fn set(on: bool) -> Result<()> {
    if !on {
        let result = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                wide(KEY).as_ptr(),
                wide("AV Relay").as_ptr(),
            )
        };
        if result != 0 && result != 2 {
            bail!("Could not disable start at login (Windows error {result})");
        }
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let bin = exe.parent().context("Missing application folder")?;
    let value = if bin.join("gstreamer-1.0-0.dll").is_file() {
        format!("\"{}\" --at-login", exe.display())
    } else {
        let script = bin
            .ancestors()
            .nth(2)
            .map(PathBuf::from)
            .context("Missing checkout folder")?
            .join("scripts/start.ps1");
        if !script.is_file() {
            bail!(
                "Install AV Relay or keep scripts/start.ps1 beside this development build to enable start at login."
            );
        }
        format!(
            "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File \"{}\" -AtLogin",
            script.display()
        )
    };
    let mut key = ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(KEY).as_ptr(),
            0,
            ptr::null(),
            0,
            KEY_SET_VALUE,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        )
    };
    if status != 0 {
        bail!("Could not open login settings (Windows error {status})");
    }
    let value = wide(&value);
    let result = unsafe {
        RegSetValueExW(
            key,
            wide("AV Relay").as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if result != 0 {
        bail!("Could not enable start at login (Windows error {result})");
    }
    Ok(())
}
