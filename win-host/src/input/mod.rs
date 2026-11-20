mod hid_scancode;
mod injector;
pub(crate) use injector::DirectExecutor;
pub use injector::Injector;

use crate::win32;
use anyhow::{ensure, Context, Result};
use std::{io, mem};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

fn foreground(hwnd: HWND) -> Result<()> {
    unsafe {
        if GetForegroundWindow() != hwnd {
            SetForegroundWindow(hwnd);
            // The foreground thread processes activation asynchronously.
            for _ in 0..10 {
                if GetForegroundWindow() == hwnd {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            ensure!(
                GetForegroundWindow() == hwnd,
                "Windows declined foreground activation"
            );
        }
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
