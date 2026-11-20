mod common;
use common::*;
use std::{
    collections::HashSet,
    mem, ptr,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use win_host::proto::{control_message::Payload, *};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::WindowsAndMessaging::*,
};

struct TestWindows {
    ids: [u64; 2],
    tid: u32,
    thread: Option<thread::JoinHandle<()>>,
}
unsafe extern "system" fn paint(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return 1;
    }
    if msg == WM_TIMER {
        InvalidateRect(h, ptr::null(), 0);
        return 0;
    }
    if msg == WM_PAINT {
        let mut ps: PAINTSTRUCT = mem::zeroed();
        let dc = BeginPaint(h, &mut ps);
        let brush = CreateSolidBrush(GetWindowLongPtrW(h, GWLP_USERDATA) as u32);
        FillRect(dc, &ps.rcPaint, brush);
        DeleteObject(brush);
        EndPaint(h, &ps);
        return 0;
    }
    DefWindowProcW(h, msg, w, l)
}
impl TestWindows {
    fn new() -> Self {
        win_host::win32::init_dpi();
        let (tx, rx) = mpsc::channel();
        let t = thread::spawn(move || unsafe {
            let class_name: Vec<u16> = format!("WinBridgeRenderTest-{}", GetCurrentThreadId())
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let class = class_name.as_ptr();
            let inst = GetModuleHandleW(ptr::null());
            let wc = WNDCLASSW {
                hInstance: inst,
                lpszClassName: class,
                lpfnWndProc: Some(paint),
                ..mem::zeroed()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let mut ids = [0u64; 2];
            for (i, c) in [0x2244cc, 0x44cc22].into_iter().enumerate() {
                let hwnd = CreateWindowExW(
                    WS_EX_NOACTIVATE,
                    class,
                    windows_sys::w!("WinBridge rendering test"),
                    WS_OVERLAPPEDWINDOW,
                    80 + (i as i32) * 250,
                    75,
                    240,
                    180,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    inst,
                    ptr::null(),
                );
                assert!(!hwnd.is_null());
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, c as isize);
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                SetTimer(hwnd, 1, 30, None);
                ids[i] = hwnd as usize as u64;
            }
            tx.send((ids, GetCurrentThreadId())).unwrap();
            let mut msg: MSG = mem::zeroed();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            for id in ids {
                DestroyWindow(id as usize as HWND);
            }
            UnregisterClassW(class, inst);
        });
        let (ids, tid) = rx.recv_timeout(Duration::from_secs(8)).unwrap();
        Self {
            ids,
            tid,
            thread: Some(t),
        }
    }
}
impl Drop for TestWindows {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.tid, WM_QUIT, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
#[test]
#[ignore = "requires an unlocked interactive Windows desktop and WGC"]
fn streams_two_windows_and_applies_visible_resize() {
    let windows = TestWindows::new();
    let host = Host::start("raw");
    let mut ctl = host.connect();
    let ack = hello(&mut ctl, TOKEN, vec![4]);
    assert!(ack.accepted);
    let (mut stream, status) = media(&host, &ack.session_id);
    assert_eq!(status, 0);
    send(
        &mut ctl,
        Payload::ListWindowsReq(ListWindowsRequest {
            include_icons: true,
        }),
    );
    loop {
        if let Payload::ListWindowsRes(list) = recv(&mut ctl) {
            assert!(windows
                .ids
                .iter()
                .all(|id| list.windows.iter().any(|w| w.window_id == *id)));
            break;
        }
    }
    for id in windows.ids {
        send(
            &mut ctl,
            Payload::SubscribeWindow(SubscribeWindow {
                window_id: id,
                target_fps: 15,
                ..Default::default()
            }),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = HashSet::new();
    while seen.len() < 2 {
        assert!(Instant::now() < deadline, "second window did not arrive");
        let f = frame(&mut stream);
        let index = windows.ids.iter().position(|&id| id == f.id).unwrap();
        assert_eq!(f.codec, 4);
        assert_eq!(f.flags, 1);
        assert_eq!(f.bytes.len(), usize::from(f.w) * usize::from(f.h) * 4);
        let offset = (usize::from(f.h / 2) * usize::from(f.w) + usize::from(f.w / 2)) * 4;
        let expected = [[0x22u8, 0x44, 0xcc], [0x44, 0xcc, 0x22]][index];
        assert_eq!(&f.bytes[offset..offset + 3], &expected);
        seen.insert(f.id);
    }
    send(
        &mut ctl,
        Payload::ResizeWindow(ResizeWindow {
            window_id: windows.ids[0],
            width: 304,
            height: 240,
            resize_seq: 19,
        }),
    );
    loop {
        match recv(&mut ctl) {
            Payload::WindowUpdated(u) if u.ack_resize_seq == 19 => {
                let actual = u.window.unwrap();
                assert_eq!((actual.width, actual.height), (304, 240));
                break;
            }
            Payload::HostError(e) => panic!("resize or capture failed: {e:?}"),
            _ => {}
        }
    }
    send(
        &mut ctl,
        Payload::UnsubscribeWindow(UnsubscribeWindow {
            window_id: windows.ids[0],
        }),
    );
    while Instant::now() < deadline {
        let f = frame(&mut stream);
        if f.flags & 4 != 0 && f.id == windows.ids[0] {
            assert!(f.bytes.is_empty());
            return;
        }
    }
    panic!("missing end-of-stream");
}

#[test]
#[ignore = "requires an unlocked Windows desktop and an H.264 encoder"]
fn h264_subscription_starts_requests_and_resizes_at_decodable_idrs() {
    let windows = TestWindows::new();
    let host = Host::start("h264");
    let mut ctl = host.connect();
    let ack = hello(&mut ctl, TOKEN, vec![1]);
    assert!(ack.accepted, "{}", ack.reject_reason);
    let (mut stream, ok) = media(&host, &ack.session_id);
    assert_eq!(ok, 0);
    let id = windows.ids[0];
    send(
        &mut ctl,
        Payload::SubscribeWindow(SubscribeWindow {
            window_id: id,
            target_fps: 30,
            ..Default::default()
        }),
    );
    let check_key = |f: &Frame| {
        assert_eq!((f.id, f.codec, f.flags & 1), (id, 1, 1));
        let types: Vec<_> = f
            .bytes
            .windows(5)
            .filter(|w| w[..4] == [0, 0, 0, 1])
            .map(|w| w[4] & 31)
            .collect();
        assert!(
            types.starts_with(&[7, 8]) && types.contains(&5),
            "{types:?}"
        );
    };
    check_key(&frame(&mut stream));
    send(
        &mut ctl,
        Payload::RequestKeyFrame(RequestKeyFrame { window_id: id }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for requested IDR"
        );
        let f = frame(&mut stream);
        if f.flags & 1 != 0 {
            check_key(&f);
            break;
        }
    }
    send(
        &mut ctl,
        Payload::ResizeWindow(ResizeWindow {
            window_id: id,
            width: 380,
            height: 264,
            resize_seq: 7,
        }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for reconfigured encoder"
        );
        let f = frame(&mut stream);
        if (f.w, f.h) == (380, 264) {
            check_key(&f);
            break;
        }
    }
}
