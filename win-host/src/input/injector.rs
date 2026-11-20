use super::{foreground, hid_scancode::scancode, keyboard, send, text};
use crate::{
    proto::{control_message::Payload, MouseEvent, WindowInfo, WindowUpdated},
    win32,
};
use anyhow::{bail, ensure, Context, Result};
use std::{collections::BTreeSet, io, mem, ptr, sync::mpsc as sync};
use tokio::sync::oneshot;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::*,
    System::Threading::*,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

type Reply = oneshot::Sender<Result<Option<WindowUpdated>>>;
pub struct Injector {
    tx: Option<sync::Sender<(WindowInfo, Payload, Reply)>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Injector {
    pub fn new() -> Result<Self> {
        let (tx, rx) = sync::channel::<(WindowInfo, Payload, Reply)>();
        let thread = std::thread::Builder::new()
            .name("window-input".into())
            .spawn(move || {
                let mut state = State::default();
                for (target, payload, reply) in rx {
                    if !reply.is_closed() {
                        let _ = reply.send(state.apply(&target, payload));
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
        })
    }
    pub async fn execute(
        &self,
        target: &WindowInfo,
        payload: Payload,
    ) -> Result<Option<WindowUpdated>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .as_ref()
            .context("input thread closed")?
            .send((target.clone(), payload, tx))
            .map_err(|_| anyhow::anyhow!("input thread closed"))?;
        rx.await?
    }
}
impl Drop for Injector {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(handle) = self.thread.take() {
            if tokio::runtime::Handle::try_current()
                .is_ok_and(|rt| rt.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
            {
                let _ = tokio::task::block_in_place(|| handle.join());
            } else {
                let _ = handle.join();
            }
        }
    }
}

fn elevated(process: HANDLE) -> Option<bool> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut result: TOKEN_ELEVATION = mem::zeroed();
        let mut n = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut result as *mut TOKEN_ELEVATION).cast(),
            mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut n,
        );
        CloseHandle(token);
        (ok != 0).then_some(result.TokenIsElevated != 0)
    }
}
fn check_access(pid: u32) -> Result<()> {
    unsafe {
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !proc.is_null() {
            let is_elevated = elevated(proc);
            CloseHandle(proc);
            ensure!(
                is_elevated != Some(true) || elevated(GetCurrentProcess()) != Some(false),
                "ACCESS_DENIED_ELEVATED: window runs with administrative privileges"
            );
        }
    }
    Ok(())
}

#[derive(Default)]
struct State {
    keys: BTreeSet<u32>,
    buttons: BTreeSet<i32>,
    wheel: [f64; 2],
}
impl Drop for State {
    fn drop(&mut self) {
        for &hid in &self.keys {
            if let Ok(key) = key_input(hid, false) {
                let _ = send(&[key]);
            }
        }
        for &btn in &self.buttons {
            if let Ok(up) = button(btn, false) {
                let _ = send(&[up]);
            }
        }
    }
}

fn mouse(flags: u32, data: u32, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
fn button(btn: i32, down: bool) -> Result<INPUT> {
    let (on, off, data) = match btn {
        1 => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        2 => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        3 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        4 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
        5 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
        _ => bail!("unsupported mouse button"),
    };
    Ok(mouse(if down { on } else { off }, data, 0, 0))
}

fn virtual_axis(position: i32, size: u32, norm: f32, origin: i32, extent: i32) -> Result<i32> {
    ensure!(
        norm.is_finite() && extent > 0,
        "invalid virtual desktop coordinates"
    );
    let pixel = f64::from(position) + (f64::from(norm) * f64::from(size)).round();
    // On the exact trailing edge select the final pixel inside the window.
    let pixel = if norm == 1.0 { pixel - 1.0 } else { pixel };
    Ok(
        ((pixel - f64::from(origin)) * 65535.0 / f64::from((extent - 1).max(1)))
            .round()
            .clamp(0., 65535.) as i32,
    )
}
fn key_input(hid: u32, down: bool) -> Result<INPUT> {
    if hid == 0x48 {
        // Pause uses a multi-byte E1 sequence and has no Set-1 keyup.
        return Ok(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_PAUSE,
                    wScan: 0,
                    dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    let (scan, extended) = scancode(hid).context("unsupported HID keyboard usage")?;
    Ok(keyboard(
        scan,
        KEYEVENTF_SCANCODE
            | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }
            | if down { 0 } else { KEYEVENTF_KEYUP },
    ))
}

impl State {
    fn mouse(&mut self, target: &WindowInfo, ev: MouseEvent) -> Result<()> {
        unsafe {
            let dx = virtual_axis(
                target.x,
                target.width,
                ev.norm_x,
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
            )?;
            let dy = virtual_axis(
                target.y,
                target.height,
                ev.norm_y,
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )?;
            let mut events = vec![mouse(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                0,
                dx,
                dy,
            )];
            match ev.kind {
                1 => {}
                2 | 3 => events.push(button(ev.button, ev.kind == 2)?),
                4 => {
                    for (i, (delta, flag)) in [
                        (ev.wheel_delta_x, MOUSEEVENTF_HWHEEL),
                        (ev.wheel_delta_y, MOUSEEVENTF_WHEEL),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        ensure!(delta.is_finite(), "non-finite wheel delta");
                        let sum = self.wheel[i] + f64::from(delta);
                        let amount = sum.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;
                        self.wheel[i] = sum - sum.round();
                        if amount != 0 {
                            events.push(mouse(flag, amount as u32, 0, 0));
                        }
                    }
                }
                _ => bail!("unsupported mouse event"),
            }
            send(&events)?;
            match ev.kind {
                2 => {
                    self.buttons.insert(ev.button);
                }
                3 => {
                    self.buttons.remove(&ev.button);
                }
                _ => {}
            }
            Ok(())
        }
    }
    fn apply(&mut self, target: &WindowInfo, payload: Payload) -> Result<Option<WindowUpdated>> {
        let current = win32::info(target.window_id)?;
        ensure!(current.pid == target.pid, "window handle has been reused");
        let hwnd = win32::checked_hwnd(current.window_id)?;
        check_access(current.pid)?;
        let focus = || -> Result<()> {
            let owner_id = if current.owner_window_id != 0 {
                current.owner_window_id
            } else {
                target.owner_window_id
            };
            let owner = if target.kind == 3 && owner_id != 0 {
                win32::checked_hwnd(owner_id)?
            } else {
                hwnd
            };
            foreground(owner)
        };
        match payload {
            Payload::ResizeWindow(r) => {
                let window = win32::resize(current.window_id, r.width, r.height)?;
                return Ok(Some(WindowUpdated {
                    window: Some(window),
                    ack_resize_seq: r.resize_seq,
                }));
            }
            Payload::FocusWindow(r) => {
                if r.focused {
                    focus()?;
                }
            }
            Payload::CloseWindow(_) => {
                if unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } == 0 {
                    return Err(io::Error::last_os_error()).context("WM_CLOSE");
                }
            }
            Payload::WindowAction(a) => match a.action {
                1 | 2 => unsafe {
                    ShowWindowAsync(
                        hwnd,
                        if a.action == 1 {
                            SW_MAXIMIZE
                        } else {
                            SW_RESTORE
                        },
                    );
                },
                3 => focus()?,
                _ => bail!("unknown window action"),
            },
            Payload::MouseEvent(ev) => {
                if ev.kind == 2 {
                    focus()?;
                }
                self.mouse(&current, ev)?;
            }
            Payload::KeyEvent(ev) => {
                focus()?;
                send(&[key_input(ev.hid_usage, ev.is_down)?])?;
                if ev.is_down {
                    self.keys.insert(ev.hid_usage);
                } else {
                    self.keys.remove(&ev.hid_usage);
                }
            }
            Payload::TextInput(t) => {
                focus()?;
                text(&t.text)?;
            }
            _ => bail!("unsupported input command"),
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::virtual_axis;
    #[test]
    fn negative_monitor_and_drag() {
        assert_eq!(virtual_axis(-1920, 1920, 0.0, -1920, 3840).unwrap(), 0);
        assert_eq!(virtual_axis(0, 1920, 1.0, -1920, 3840).unwrap(), 65535);
        assert!(
            virtual_axis(100, 800, -0.1, 0, 1920).unwrap()
                < virtual_axis(100, 800, 0.0, 0, 1920).unwrap()
        );
    }
}
