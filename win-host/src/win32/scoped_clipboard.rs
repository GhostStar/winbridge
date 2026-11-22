//! Clipboard access only on behalf of a live subscribed window. No background export.
use crate::proto::{ClipboardRequest, ClipboardSync};
use anyhow::{ensure, Context, Result};
use std::{
    collections::HashMap,
    mem, thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    System::DataExchange::{GetClipboardOwner, GetClipboardSequenceNumber},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

#[derive(Clone)]
pub(crate) struct Scope {
    pub id: u64,
    pub pid: u32,
    pub started: u64,
    pub allowed: Vec<(u32, u64)>,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        let current = super::fast_bounds(self.id)?;
        ensure!(
            current.pid == self.pid
                && self.started != 0
                && super::processes::started(self.pid) == Some(self.started),
            "clipboard target changed"
        );
        let hwnd = super::checked_hwnd(self.id)?;
        ensure!(
            unsafe { IsWindowEnabled(hwnd) } != 0,
            "clipboard target is disabled"
        );
        ensure!(
            super::foreground_id() == self.id,
            "clipboard target is not foreground"
        );
        ensure!(
            !super::desktop::is_secure_desktop(),
            "clipboard unavailable on secure desktop"
        );
        Ok(())
    }
    pub fn owns_clipboard(&self) -> bool {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(GetClipboardOwner(), &mut pid);
        }
        pid != 0
            && self.allowed.iter().any(|(p, started)| {
                *p == pid && *started != 0 && super::processes::started(pid) == Some(*started)
            })
    }
    fn activate(&self) -> Result<()> {
        // Do not let a reused handle gain focus before checking its process identity.
        let current = super::fast_bounds(self.id)?;
        ensure!(
            current.pid == self.pid
                && self.started != 0
                && super::processes::started(self.pid) == Some(self.started),
            "clipboard target changed"
        );
        let hwnd = super::checked_hwnd(self.id)?;
        ensure!(
            unsafe { IsWindowEnabled(hwnd) } != 0,
            "clipboard target is disabled"
        );
        unsafe {
            SetForegroundWindow(hwnd);
        }
        self.validate()
    }
}

pub(crate) struct Command {
    pub request: ClipboardRequest,
    pub scope: Scope,
    pub deadline: Instant,
    pub reply: tokio::sync::oneshot::Sender<Result<Option<ClipboardSync>>>,
}
#[derive(Default)]
pub(crate) struct Transactions {
    baselines: HashMap<(u64, u64), u32>,
}
impl Transactions {
    pub fn execute(
        &mut self,
        hwnd: HWND,
        request: ClipboardRequest,
        scope: Scope,
    ) -> Result<Option<ClipboardSync>> {
        ensure!(
            matches!(request.action.as_str(), "copy" | "cut" | "paste" | "poll"),
            "unknown clipboard action"
        );
        ensure!(
            request.action == "paste" || request.data.is_none(),
            "unexpected clipboard payload"
        );
        let key = (request.window_id, request.token);
        self.baselines
            .retain(|(id, token), _| *id != request.window_id || *token == request.token);
        if self.baselines.len() >= 64 && !self.baselines.contains_key(&key) {
            self.baselines.clear();
        }
        if request.action == "poll" {
            scope.validate()?;
            let current = unsafe { GetClipboardSequenceNumber() };
            let previous = self.baselines.insert(key, current);
            if request.discard || previous.is_none() || previous == Some(current) {
                return Ok(None);
            }
            return super::clipboard::read(hwnd, &scope);
        }
        scope.activate()?;
        let before = unsafe { GetClipboardSequenceNumber() };
        if request.action == "paste" {
            super::clipboard::write(hwnd, request.data.context("missing paste data")?, &scope)?;
        }
        // Writing completed, clipboard lock released, and exact focus rechecked
        // before injecting the complete shortcut in a single SendInput batch.
        scope.validate()?;
        shortcut(match request.action.as_str() {
            "copy" => 0x43,
            "cut" => 0x58,
            _ => 0x56,
        })?;
        if request.action == "paste" {
            self.baselines
                .insert(key, unsafe { GetClipboardSequenceNumber() });
            return Ok(None);
        }
        let until = Instant::now() + Duration::from_millis(500);
        while unsafe { GetClipboardSequenceNumber() } == before {
            scope.validate()?;
            if Instant::now() >= until {
                return Ok(None);
            }
            // EmptyClipboard sends WM_DESTROYCLIPBOARD to our previous owner
            // HWND. Service sent messages while waiting, without dispatching
            // queued commands (which would reenter the transaction worker).
            unsafe {
                let mut msg = mem::zeroed();
                PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
            }
            thread::sleep(Duration::from_millis(10));
        }
        scope.validate()?;
        self.baselines
            .insert(key, unsafe { GetClipboardSequenceNumber() });
        super::clipboard::read(hwnd, &scope)
    }
}
fn shortcut(vk: u16) -> Result<()> {
    let key = |vk, up| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let events = [
        key(VK_CONTROL, false),
        key(vk, false),
        key(vk, true),
        key(VK_CONTROL, true),
    ];
    let sent = unsafe {
        SendInput(
            events.len() as u32,
            events.as_ptr(),
            mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != events.len() as u32 {
        let releases = [key(vk, true), key(VK_CONTROL, true)];
        unsafe {
            SendInput(2, releases.as_ptr(), mem::size_of::<INPUT>() as i32);
        }
    }
    ensure!(
        sent == events.len() as u32,
        "clipboard shortcut injection failed"
    );
    Ok(())
}
