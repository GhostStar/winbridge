mod common;
use common::*;
use std::{
    mem,
    net::TcpStream,
    ptr,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use win_host::proto::{control_message::Payload, SubscribeWindow};
use windows_sys::Win32::{
    Foundation::{HWND, POINT},
    UI::WindowsAndMessaging::*,
};

// The fixture owns both HWNDs on a pumping thread. It never moves the user's
// pointer or activates a window. The popup appears briefly under the pointer.
struct Windows {
    owner: u64,
    popup: u64,
    thread: Option<thread::JoinHandle<()>>,
}
impl Windows {
    fn new() -> Self {
        let (send, recv) = mpsc::channel();
        let thread = thread::spawn(move || unsafe {
            let class = windows_sys::w!("STATIC");
            let owner = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class,
                windows_sys::w!("WinBridge popup fixture"),
                WS_OVERLAPPEDWINDOW,
                64,
                64,
                320,
                240,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            assert!(!owner.is_null());
            let popup = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                class,
                windows_sys::w!("WinBridge menu"),
                WS_POPUP | WS_BORDER,
                96,
                96,
                150,
                90,
                owner,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            assert!(!popup.is_null());
            ShowWindow(owner, SW_SHOWNOACTIVATE);
            send.send((owner as usize as u64, popup as usize as u64))
                .unwrap();
            let mut msg = mem::zeroed();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                match msg.message {
                    WM_APP => {
                        let mut point: POINT = mem::zeroed();
                        assert_ne!(GetCursorPos(&mut point), 0);
                        assert_ne!(
                            SetWindowPos(
                                popup,
                                HWND_TOPMOST,
                                point.x - 48,
                                point.y - 36,
                                150,
                                90,
                                SWP_NOACTIVATE | SWP_SHOWWINDOW
                            ),
                            0
                        );
                    }
                    n if n == WM_APP + 1 => {
                        ShowWindow(popup, SW_HIDE);
                    }
                    n if n == WM_APP + 2 => break,
                    n if n == WM_APP + 3 => {
                        ShowWindow(popup, SW_SHOWNOACTIVATE);
                        ShowWindow(popup, SW_HIDE);
                    }
                    _ => {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
            DestroyWindow(popup);
            DestroyWindow(owner);
        });
        let (owner, popup) = recv.recv_timeout(Duration::from_secs(4)).unwrap();
        Self {
            owner,
            popup,
            thread: Some(thread),
        }
    }
    fn command(&self, offset: u32) {
        unsafe {
            PostMessageW(self.owner as usize as HWND, WM_APP + offset, 0, 0);
        }
    }
}
impl Drop for Windows {
    fn drop(&mut self) {
        self.command(2);
        let _ = self.thread.take().unwrap().join();
    }
}

fn receive_until(socket: &mut TcpStream, check: impl Fn(Payload) -> bool) {
    let end = Instant::now() + Duration::from_secs(7);
    while Instant::now() < end {
        let message = recv(socket);
        if let Payload::HostError(ref e) = message {
            panic!(
                "unexpected host error on HWND {:#x}: {}",
                e.window_id, e.message
            );
        }
        if check(message) {
            return;
        }
    }
    panic!("timed out waiting for desktop event");
}
fn popup_frame(media: &mut TcpStream, popup: u64, eos: bool) {
    let end = Instant::now() + Duration::from_secs(7);
    while Instant::now() < end {
        let f = frame(media);
        if f.id != popup || (f.flags & 4 != 0) != eos {
            continue;
        }
        if eos {
            assert!(f.bytes.is_empty());
        } else {
            assert_eq!(f.codec, 3);
            assert_eq!(f.flags & 1, 1);
            assert_eq!(f.bytes.get(..2), Some(&[0xff, 0xd8][..]));
        }
        return;
    }
    panic!("timed out waiting for popup media, eos={eos}");
}

#[test]
#[ignore = "requires a Windows interactive desktop with WGC, shows a temporary owned popup"]
fn owned_popup_frames_cursor_hide_and_reopen() {
    win_host::win32::init_dpi();
    let windows = Windows::new();
    let host = Host::start("jpeg");
    let mut ctl = host.connect();
    let ack = hello(&mut ctl, TOKEN, vec![3]);
    assert!(ack.accepted);
    let (mut stream, status) = media(&host, &ack.session_id);
    assert_eq!(status, 0);
    send(
        &mut ctl,
        Payload::SubscribeWindow(SubscribeWindow {
            window_id: windows.owner,
            follow_popups: true,
            ..Default::default()
        }),
    );
    receive_until(
        &mut ctl,
        |p| matches!(p,Payload::WindowUpdated(ref u) if u.window.as_ref().is_some_and(|w|w.window_id==windows.owner)),
    );
    let owner = win_host::win32::info(windows.owner).unwrap();

    // An already-vanished transient must not leave a phantom subscription.
    windows.command(3);
    thread::sleep(Duration::from_millis(100));
    for cycle in 0..2 {
        windows.command(0);
        let mut updated = false;
        let mut cursor = false;
        let end = Instant::now() + Duration::from_secs(7);
        while !updated || !cursor {
            assert!(
                Instant::now() < end,
                "popup state missing: metadata={updated} cursor={cursor}"
            );
            match recv(&mut ctl) {
                Payload::WindowUpdated(u)
                    if u.window
                        .as_ref()
                        .is_some_and(|w| w.window_id == windows.popup) =>
                {
                    let w = u.window.unwrap();
                    assert_eq!((w.kind, w.owner_window_id), (3, windows.owner));
                    assert_eq!((w.offset_x, w.offset_y), (w.x - owner.x, w.y - owner.y));
                    updated = true;
                }
                Payload::CursorShape(c) if c.window_id == windows.popup => {
                    assert!(c.cursor_type > 0);
                    cursor = true;
                }
                Payload::HostError(e) => panic!("popup cycle {cycle}: {}", e.message),
                _ => {}
            }
        }
        popup_frame(&mut stream, windows.popup, false);
        windows.command(1);
        receive_until(
            &mut ctl,
            |p| matches!(p,Payload::WindowClosed(ref c) if c.window_id==windows.popup),
        );
        popup_frame(&mut stream, windows.popup, true);
        thread::sleep(Duration::from_millis(100));
    }
}
