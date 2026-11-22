//! Interactive, synthetic chat/image-viewer regression. No user app is inspected.
mod common;
use common::*;
use std::{
    io::Write,
    mem, ptr,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use win_host::proto::{control_message::Payload, *};
use windows_sys::Win32::{
    Foundation::{HWND, POINT, SIZE},
    Graphics::Gdi::*,
    UI::{Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
};
struct Fixture {
    root: u64,
    viewer: u64,
    panel: u64,
    thread: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || unsafe {
            let root = CreateWindowExW(
                0,
                windows_sys::w!("STATIC"),
                windows_sys::w!("Synthetic chat"),
                WS_OVERLAPPEDWINDOW,
                60,
                60,
                400,
                300,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            let viewer = CreateWindowExW(
                WS_EX_LAYERED,
                windows_sys::w!("STATIC"),
                windows_sys::w!("Synthetic photo"),
                WS_POPUP,
                120,
                120,
                128,
                96,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            let panel = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                windows_sys::w!("STATIC"),
                windows_sys::w!("Synthetic dialog"),
                WS_POPUP | WS_CAPTION,
                150,
                150,
                200,
                120,
                root,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            assert!(!root.is_null() && !viewer.is_null() && !panel.is_null());
            ShowWindow(root, SW_SHOW);
            SetForegroundWindow(root);
            tx.send((
                root as usize as u64,
                viewer as usize as u64,
                panel as usize as u64,
            ))
            .unwrap();
            let mut msg = mem::zeroed();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                if msg.message == WM_APP {
                    paint(viewer);
                    ShowWindow(viewer, SW_SHOW);
                    SetForegroundWindow(viewer);
                } else if msg.message == WM_APP + 1 {
                    ShowWindow(viewer, SW_HIDE);
                } else if msg.message == WM_APP + 2 {
                    break;
                } else if msg.message == WM_APP + 3 {
                    ShowWindow(root, SW_HIDE);
                } else if msg.message == WM_APP + 4 {
                    EnableWindow(root, 0);
                    ShowWindow(panel, SW_SHOW);
                    SetForegroundWindow(panel);
                } else {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            DestroyWindow(panel);
            DestroyWindow(viewer);
            DestroyWindow(root);
        });
        let (root, viewer, panel) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            root,
            viewer,
            panel,
            thread: Some(thread),
        }
    }
    fn command(&self, n: u32) {
        unsafe {
            PostMessageW(self.root as usize as HWND, WM_APP + n, 0, 0);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.command(2);
        let _ = self.thread.take().unwrap().join();
    }
}
unsafe fn paint(hwnd: HWND) {
    let screen = GetDC(ptr::null_mut());
    let dc = CreateCompatibleDC(screen);
    let mut info: BITMAPINFO = mem::zeroed();
    info.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = 128;
    info.bmiHeader.biHeight = -96;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    let mut bits = ptr::null_mut();
    let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
    assert!(!bitmap.is_null());
    let old = SelectObject(dc, bitmap);
    let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, 128 * 96 * 4);
    for y in 0..96 {
        for x in 0..128 {
            let a = if (16..112).contains(&x) && (16..80).contains(&y) {
                255
            } else if (8..120).contains(&x) && (8..88).contains(&y) {
                128
            } else {
                0
            };
            let c = if x < 64 { 0 } else { a };
            pixels[(y * 128 + x) * 4..(y * 128 + x) * 4 + 4].copy_from_slice(&[c, c, c, a]);
        }
    }
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    assert_ne!(
        UpdateLayeredWindow(
            hwnd,
            screen,
            &POINT { x: 120, y: 120 },
            &SIZE { cx: 128, cy: 96 },
            dc,
            &POINT { x: 0, y: 0 },
            0,
            &blend,
            ULW_ALPHA
        ),
        0
    );
    SelectObject(dc, old);
    DeleteObject(bitmap);
    DeleteDC(dc);
    ReleaseDC(ptr::null_mut(), screen);
}
fn until(ctl: &mut std::net::TcpStream, predicate: impl Fn(&Payload) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let p = recv(ctl);
        assert!(
            !matches!(p, Payload::HostError(_)),
            "synthetic host error: {p:?}"
        );
        if predicate(&p) {
            return;
        }
        assert!(Instant::now() < deadline, "synthetic event timed out");
    }
}
#[test]
#[ignore = "requires interactive Windows WGC; creates and focuses only synthetic windows"]
fn ownerless_image_viewer_streams_alpha_and_reopens() {
    exercise("raw", 4, true);
}

#[test]
#[ignore = "requires interactive WGC and a working Media Foundation H.264 encoder"]
fn ownerless_image_viewer_keeps_h264_color_with_alpha() {
    assert!(win_host::capture::supports_h264());
    exercise("h264", 1, true);
}

#[test]
#[ignore = "requires interactive WGC; verifies legacy client framing"]
fn old_client_receives_no_alpha_envelope() {
    exercise("raw", 4, false);
}

fn exercise(codec: &str, wire_codec: i32, with_alpha: bool) {
    win_host::win32::init_dpi();
    let fixture = Fixture::new();
    let host = Host::start(codec);
    let mut ctl = host.connect();
    ctl.write_all(b"WBRGCTL\x01").unwrap();
    send(
        &mut ctl,
        Payload::Hello(Hello {
            protocol_version: 1,
            auth_token: TOKEN.into(),
            supported_codecs: vec![wire_codec],
            capabilities: if with_alpha { 1 << 8 } else { 0 },
            ..Default::default()
        }),
    );
    let Payload::HelloAck(ack) = recv(&mut ctl) else {
        panic!("missing hello")
    };
    assert!(ack.accepted);
    let (mut video, status) = media(&host, &ack.session_id);
    assert_eq!(status, 0);
    send(
        &mut ctl,
        Payload::SubscribeWindow(SubscribeWindow {
            window_id: fixture.root,
            follow_popups: true,
            target_fps: 10,
            ..Default::default()
        }),
    );
    until(
        &mut ctl,
        |p| matches!(p,Payload::WindowUpdated(u) if u.window.as_ref().is_some_and(|w|w.window_id==fixture.root)),
    );
    for _ in 0..2 {
        fixture.command(0);
        until(
            &mut ctl,
            |p| matches!(p,Payload::WindowUpdated(u) if u.window.as_ref().is_some_and(|w|w.window_id==fixture.viewer && w.kind==1 && w.owner_window_id==fixture.root)),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let frame = frame(&mut video);
            if frame.id == fixture.viewer && frame.flags & 4 == 0 {
                if !with_alpha {
                    assert_eq!(frame.flags & 8, 0);
                    break;
                }
                assert_ne!(frame.flags & 8, 0, "layered WGC frame lost alpha");
                let count = u32::from_be_bytes(frame.bytes[..4].try_into().unwrap()) as usize;
                if wire_codec == 4 {
                    assert_eq!(count, usize::from(frame.w) * usize::from(frame.h) * 4);
                }
                let png = &frame.bytes[4 + count..];
                let mut reader = png::Decoder::new(png).read_info().unwrap();
                let mut alpha = vec![0; reader.output_buffer_size()];
                reader.next_frame(&mut alpha).unwrap();
                assert!(alpha.contains(&0) && alpha.contains(&128) && alpha.contains(&255));
                let color = &frame.bytes[4..4 + count];
                if wire_codec == 4 {
                    assert!(
                        color
                            .chunks_exact(4)
                            .zip(&alpha)
                            .any(|(p, a)| p[..3] == [0, 0, 0] && *a == 255),
                        "opaque black content was removed"
                    );
                    for (p, a) in color.chunks_exact(4).zip(&alpha) {
                        assert_eq!(p[3], *a);
                        assert!(p[..3].iter().all(|c| c <= a));
                    }
                } else {
                    assert_eq!(frame.codec, 1);
                    assert_ne!(frame.flags & 1, 0);
                    assert!(color.starts_with(&[0, 0, 0, 1]) || color.starts_with(&[0, 0, 1]));
                    if let Some(path) = std::env::var_os("WINBRIDGE_TEST_FRAME_OUT") {
                        // Export only fixture pixels for the matching Mac decoder regression.
                        let mut bytes = Vec::new();
                        bytes.extend_from_slice(&frame.w.to_be_bytes());
                        bytes.extend_from_slice(&frame.h.to_be_bytes());
                        bytes.extend_from_slice(&frame.bytes);
                        std::fs::write(path, bytes).unwrap();
                    }
                }
                break;
            }
            assert!(Instant::now() < deadline);
        }
        fixture.command(1);
        until(
            &mut ctl,
            |p| matches!(p,Payload::WindowClosed(w) if w.window_id==fixture.viewer),
        );
        // Control and media use separate streams: retire this generation's EOS
        // before checking that a later reopened viewer remains alive.
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let ended = frame(&mut video);
            if ended.id == fixture.viewer && ended.flags & 4 != 0 {
                break;
            }
            assert!(Instant::now() < deadline);
        }
    }
    fixture.command(0);
    until(
        &mut ctl,
        |p| matches!(p,Payload::WindowUpdated(u) if u.window.as_ref().is_some_and(|w|w.window_id==fixture.viewer)),
    );
    fixture.command(3);
    until(
        &mut ctl,
        |p| matches!(p,Payload::WindowClosed(w) if w.window_id==fixture.root),
    );
    send(
        &mut ctl,
        Payload::RequestKeyFrame(RequestKeyFrame {
            window_id: fixture.viewer,
        }),
    );
    // A control barrier ensures the independent member is still subscribed.
    send(
        &mut ctl,
        Payload::ListWindowsReq(ListWindowsRequest {
            include_icons: false,
        }),
    );
    until(&mut ctl, |p| matches!(p, Payload::ListWindowsRes(_)));
    // The fixture is static: WGC need not produce more frames until requested.
    // Request fresh frames after the control barrier instead of expecting motion.
    for _ in 0..3 {
        send(
            &mut ctl,
            Payload::RequestKeyFrame(RequestKeyFrame {
                window_id: fixture.viewer,
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let next = frame(&mut video);
            if next.id == fixture.viewer {
                assert_eq!(
                    next.flags & 4,
                    0,
                    "independent viewer ended with capture root"
                );
                if !next.bytes.is_empty() {
                    break;
                }
            }
            assert!(Instant::now() < deadline);
        }
    }
}

#[tokio::test]
#[ignore = "requires an isolated interactive Windows desktop"]
async fn native_owned_modal_blocks_host_input_but_remains_visible() {
    let fixture = Fixture::new();
    fixture.command(4);
    let deadline = Instant::now() + Duration::from_secs(5);
    let panel = loop {
        if let Ok(info) = win_host::win32::info(fixture.panel) {
            if info.is_modal {
                break info;
            }
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(panel.kind, 2);
    assert_eq!(panel.owner_window_id, fixture.root);
    assert!(!panel.is_disabled);
    let root = win_host::win32::info(fixture.root).unwrap();
    assert!(root.is_disabled);
    let injector = win_host::input::Injector::new().unwrap();
    let result = injector
        .execute(
            &root,
            Payload::CloseWindow(CloseWindow {
                window_id: fixture.root,
            }),
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("disabled"));
    assert!(win_host::win32::info(fixture.root).is_ok());
}
