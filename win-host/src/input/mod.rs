pub mod gamepad;
mod hid_scancode;
mod injector;
pub use gamepad::GamepadManager;
pub(crate) use injector::DirectExecutor;
pub use injector::Injector;

use crate::win32;
use anyhow::{ensure, Context, Result};
use std::{io, mem};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

fn is_foreground_target(hwnd: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg == hwnd {
            return true;
        }
        if fg.is_null() {
            return false;
        }
        // Ownership / a shared PID is not focus: a hidden-on-Mac sibling may
        // still be the foreground Windows surface. Only actual child controls
        // of this target are equivalent.
        IsChild(hwnd, fg) != 0
    }
}

/// Gently raises and focuses `hwnd` without cross-process `AttachThreadInput` or
/// `HWND_TOPMOST` toggling (both of which can trigger anti-automation risk controls
/// or UI message-pump glitches in apps such as WeChat).
fn foreground(hwnd: HWND) -> Result<()> {
    unsafe {
        if is_foreground_target(hwnd) {
            return Ok(());
        }
        if IsIconic(hwnd) != 0 {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        let _ = SetWindowPos(
            hwnd,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if is_foreground_target(hwnd) {
            return Ok(());
        }
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            if is_foreground_target(hwnd) {
                return Ok(());
            }
        }
        ensure!(
            is_foreground_target(hwnd),
            "Windows declined foreground activation"
        );
        Ok(())
    }
}
fn send(inputs: &[INPUT]) -> Result<()> {
    let sent = unsafe {
        SendInput(
            inputs.len().try_into()?,
            inputs.as_ptr(),
            mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        return Err(io::Error::last_os_error()).context("SendInput did not inject all events");
    }
    Ok(())
}
fn keyboard(scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
fn text(value: &str) -> Result<()> {
    let mut batch = Vec::with_capacity(512);
    for unit in value.encode_utf16() {
        batch.push(keyboard(unit, KEYEVENTF_UNICODE));
        batch.push(keyboard(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        if batch.len() == 512 {
            send(&batch)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        send(&batch)?;
    }
    Ok(())
}

pub fn probe(id: u64) -> Result<()> {
    let win = win32::info(id)?;
    ensure!(
        win.is_resizable,
        "input probe needs a resizable target window"
    );
    foreground(win32::checked_hwnd(id)?)?;
    text("WinBridge 中文输入")?;
    let updated = win32::resize(
        id,
        win.width.saturating_add(16),
        win.height.saturating_add(16),
    )?;
    println!(
        "Input probe requested Unicode text and resize; visible size {}x{}",
        updated.width, updated.height
    );
    Ok(())
}
