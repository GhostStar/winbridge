//! Synthetic clipboard transactions. Interactive checks require clipboard VM sharing OFF.
mod common;
use common::*;
use std::{
    io::{BufRead, Read, Write},
    mem,
    process::{Command, Stdio},
    ptr,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use win_host::proto::{control_message::Payload, *};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{DataExchange::*, Memory::*},
    UI::WindowsAndMessaging::*,
};

fn response(stream: &mut std::net::TcpStream, predicate: impl Fn(&Payload) -> bool) -> Payload {
    for _ in 0..200 {
        let p = recv(stream);
        assert!(
            !matches!(
                p,
                Payload::ClipboardSync(_) | Payload::ClipboardFilesNotice(_)
            ),
            "unsolicited clipboard export"
        );
        if predicate(&p) {
            return p;
        }
    }
    panic!("expected response missing")
}
fn transact(stream: &mut std::net::TcpStream, request: ClipboardRequest) -> ClipboardResult {
    let id = request.request_id;
    send(stream, Payload::ClipboardRequest(request));
    let Payload::ClipboardResult(result) = response(
        stream,
        |p| matches!(p,Payload::ClipboardResult(r) if r.request_id == id),
    ) else {
        unreachable!()
    };
    result
}
#[test]
fn rejects_global_and_unsubscribed_clipboard_access() {
    let host = Host::start("raw");
    let mut control = host.connect();
    let ack = hello(&mut control, TOKEN, vec![4]);
    assert!(ack.accepted && ack.capabilities & (1 << 9) != 0);
    let (_video, status) = media(&host, &ack.session_id);
    assert_eq!(status, 0);
    send(
        &mut control,
        Payload::ClipboardSync(ClipboardSync {
            change_id: 1,
            content: Some(clipboard_sync::Content::TextUtf8(
                "synthetic rejected".into(),
            )),
        }),
    );
    response(
        &mut control,
        |p| matches!(p, Payload::HostError(e) if e.message.contains("Global clipboard")),
    );
    let denied = transact(
        &mut control,
        ClipboardRequest {
            request_id: 1,
            window_id: 123,
            token: 1,
            action: "poll".into(),
            ..Default::default()
        },
    );
    assert!(!denied.success && denied.data.is_none());
    send(
        &mut control,
        Payload::FilePullReq(FilePullRequest {
            request_id: 4,
            path: "clipboard:".into(),
        }),
    );
    response(
        &mut control,
        |p| matches!(p,Payload::FileTransferRes(r) if r.transfer_id==4 && !r.success),
    );
}

struct Editor {
    id: u64,
    thread: Option<thread::JoinHandle<()>>,
}
impl Editor {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || unsafe {
            let hwnd = CreateWindowExW(
                0,
                windows_sys::w!("EDIT"),
                windows_sys::w!("synthetic initial"),
                WS_OVERLAPPEDWINDOW | ES_MULTILINE as u32,
                80,
                80,
                480,
                240,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            assert!(!hwnd.is_null());
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            tx.send(hwnd as usize as u64).unwrap();
            let mut msg = mem::zeroed();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                if msg.message == WM_APP + 9 {
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // Clear only the isolated test VM's synthesized clipboard contents.
            if OpenClipboard(hwnd) != 0 {
                EmptyClipboard();
                CloseClipboard();
            }
            DestroyWindow(hwnd);
        });
        Self {
            id: rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            thread: Some(thread),
        }
    }
    fn select_all(&self) {
        unsafe {
            SendMessageW(self.id as usize as HWND, 0x00B1 /* EM_SETSEL */, 0, -1);
        }
    }
    fn text(&self) -> String {
        unsafe {
            let mut data = [0u16; 512];
            let n = GetWindowTextW(
                self.id as usize as HWND,
                data.as_mut_ptr(),
                data.len() as i32,
            );
            String::from_utf16_lossy(&data[..n as usize])
        }
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.id as usize as HWND, WM_APP + 9, 0, 0);
        }
        let _ = self.thread.take().unwrap().join();
    }
}

#[test]
#[ignore = "requires isolated interactive desktop with host/guest clipboard sharing disabled"]
fn native_scoped_copy_paste_and_foreign_owner_rejection() {
    let editor = Editor::new();
    let host = Host::start("raw");
    let mut control = host.connect();
    let ack = hello(&mut control, TOKEN, vec![4]);
    let (_video, status) = media(&host, &ack.session_id);
    assert_eq!(status, 0);
    send(
        &mut control,
        Payload::SubscribeWindow(SubscribeWindow {
            window_id: editor.id,
            target_fps: 2,
            ..Default::default()
        }),
    );
    let Payload::WindowUpdated(update) = response(
        &mut control,
        |p| matches!(p,Payload::WindowUpdated(u) if u.window.as_ref().is_some_and(|w|w.window_id==editor.id)),
    ) else {
        unreachable!()
    };
    let token = update.window.unwrap().clipboard_token;
    assert_ne!(token, 0);
    let mut id = 0;
    let mut request = |action: &str| {
        id += 1;
        ClipboardRequest {
            request_id: id,
            window_id: editor.id,
            token,
            action: action.into(),
            ..Default::default()
        }
    };
    let mut stale = request("paste");
    stale.token += 1;
    assert!(!transact(&mut control, stale).success);
    editor.select_all();
    let mut paste = request("paste");
    paste.data = Some(ClipboardSync {
        change_id: 0,
        content: Some(clipboard_sync::Content::TextUtf8(
            "scoped synthetic paste".into(),
        )),
    });
    let result = transact(&mut control, paste.clone());
    assert!(result.success, "{}", result.error);
    let deadline = Instant::now() + Duration::from_secs(3);
    while editor.text() != "scoped synthetic paste" {
        assert!(
            Instant::now() < deadline,
            "paste did not reach the intended editor"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !transact(&mut control, paste).success,
        "replayed transaction accepted"
    );
    editor.select_all();
    let copy = transact(&mut control, request("copy"));
    assert!(copy.success, "{}", copy.error);
    assert_eq!(
        copy.data.and_then(|v| v.content),
        Some(clipboard_sync::Content::TextUtf8(
            "scoped synthetic paste".into()
        ))
    );
    let before_invalid = unsafe { GetClipboardSequenceNumber() };
    let mut invalid = request("paste");
    invalid.data = Some(ClipboardSync {
        change_id: 0,
        content: Some(clipboard_sync::Content::ImagePng(vec![1, 2, 3])),
    });
    assert!(!transact(&mut control, invalid).success);
    assert_eq!(
        unsafe { GetClipboardSequenceNumber() },
        before_invalid,
        "invalid payload cleared the clipboard"
    );
    editor.select_all();
    let cut = transact(&mut control, request("cut"));
    assert!(cut.success, "{}", cut.error);
    assert_eq!(
        cut.data.and_then(|v| v.content),
        Some(clipboard_sync::Content::TextUtf8(
            "scoped synthetic paste".into()
        ))
    );
    assert!(editor.text().is_empty(), "cut did not remove selected text");
    // Legacy polling is rejected even for a valid, foreground subscription.
    let fetched = transact(&mut control, request("fetch"));
    assert!(fetched.success, "{}", fetched.error);
    assert_eq!(
        fetched.data.and_then(|v| v.content),
        Some(clipboard_sync::Content::TextUtf8(
            "scoped synthetic paste".into()
        ))
    );
    // A separate, unsubscribed process must never supply explicit fetch results.
    let mut baseline = request("poll");
    baseline.discard = true;
    let rejected_poll = transact(&mut control, baseline);
    assert!(!rejected_poll.success && rejected_poll.data.is_none());
    assert!(rejected_poll.error.contains("polling is disabled"));
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "clipboard_foreign_owner_helper",
            "--nocapture",
        ])
        .env("WINBRIDGE_CLIPBOARD_HELPER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(helper.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(output.read_line(&mut line).unwrap() > 0);
        if line.contains("synthetic helper ready") {
            break;
        }
    }
    let polled = transact(&mut control, request("fetch"));
    // Always stop helper before asserting, including a failed privacy check.
    drop(helper.stdin.take());
    assert!(helper.wait().unwrap().success());
    assert!(polled.success, "{}", polled.error);
    assert!(
        polled.data.is_none(),
        "unrelated process clipboard exported"
    );
    send(
        &mut control,
        Payload::UnsubscribeWindow(UnsubscribeWindow {
            window_id: editor.id,
        }),
    );
    let rejected = transact(&mut control, request("copy"));
    assert!(!rejected.success && rejected.data.is_none());
}

#[test]
#[ignore = "helper subprocess only"]
fn clipboard_foreign_owner_helper() {
    if std::env::var("WINBRIDGE_CLIPBOARD_HELPER").as_deref() != Ok("1") {
        return;
    }
    unsafe {
        let owner = CreateWindowExW(
            0,
            windows_sys::w!("STATIC"),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        let text: Vec<u16> = "synthetic unrelated process\0".encode_utf16().collect();
        let memory = GlobalAlloc(GMEM_MOVEABLE, text.len() * 2);
        assert!(!memory.is_null());
        let bytes = GlobalLock(memory);
        assert!(!bytes.is_null());
        ptr::copy_nonoverlapping(text.as_ptr().cast::<u8>(), bytes.cast(), text.len() * 2);
        GlobalUnlock(memory);
        assert_ne!(OpenClipboard(owner), 0);
        assert_ne!(EmptyClipboard(), 0);
        if SetClipboardData(13, memory).is_null() {
            GlobalFree(memory);
            panic!("fixture clipboard write failed");
        }
        CloseClipboard();
        println!("synthetic helper ready");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read(&mut [0u8; 1]);
        DestroyWindow(owner);
    }
}
