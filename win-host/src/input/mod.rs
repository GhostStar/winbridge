mod hid_scancode;
mod injector;
pub(crate) use injector::DirectExecutor;
pub use injector::Injector;

use crate::win32;
use anyhow::{ensure, Context, Result};
use std::{io, mem, ptr};
use windows_sys::Win32::{
    Foundation::HWND,
    System::Threading::{AttachThreadInput, GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

fn is_foreground_or_same_process(hwnd: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg == hwnd {
            return true;
        }
        if fg.is_null() {
            return false;
        }
        let mut fg_pid = 0u32;
        let mut target_pid = 0u32;
        GetWindowThreadProcessId(fg, &mut fg_pid);
        GetWindowThreadProcessId(hwnd, &mut target_pid);
        fg_pid != 0 && fg_pid == target_pid
    }
}

fn foreground(hwnd: HWND) -> Result<()> {
    unsafe {
        if is_foreground_or_same_process(hwnd) {
            return Ok(());
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
        if is_foreground_or_same_process(hwnd) {
            return Ok(());
        }

        // Temporarily attach thread input queues to bypass Windows' Foreground Lock Timeout
        // when activating a target window from a background host process.
        let cur_tid = GetCurrentThreadId();
        let fg = GetForegroundWindow();
        let fg_tid = if !fg.is_null() {
            GetWindowThreadProcessId(fg, ptr::null_mut())
        } else {
            0
        };
        let target_tid = GetWindowThreadProcessId(hwnd, ptr::null_mut());

        let attached_fg =
            fg_tid != 0 && fg_tid != cur_tid && AttachThreadInput(cur_tid, fg_tid, 1) != 0;
        let attached_target = target_tid != 0
            && target_tid != cur_tid
            && target_tid != fg_tid
            && AttachThreadInput(cur_tid, target_tid, 1) != 0;

        let was_topmost = (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOPMOST != 0;
        if !was_topmost {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let _ = SetWindowPos(
                hwnd,
                HWND_NOTOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);

        if attached_target {
            let _ = AttachThreadInput(cur_tid, target_tid, 0);
        }
        if attached_fg {
            let _ = AttachThreadInput(cur_tid, fg_tid, 0);
        }

        // The foreground thread processes activation asynchronously.
        for _ in 0..10 {
            if is_foreground_or_same_process(hwnd) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        ensure!(
            is_foreground_or_same_process(hwnd),
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
