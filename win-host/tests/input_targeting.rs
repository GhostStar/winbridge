//! Real SendInput targeting regression. Only synthetic windows receive input.
use std::{
    mem, ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use win_host::{
    input::Injector,
    proto::{control_message::Payload, MouseEvent},
    win32,
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    UI::WindowsAndMessaging::*,
};

static INPUTS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if matches!(msg, WM_LBUTTONDOWN | WM_MOUSEWHEEL) {
        INPUTS.fetch_add(1, Ordering::SeqCst);
    }
    if msg == WM_APP + 9 {
        PostQuitMessage(0);
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}
struct Fixture {
    root: u64,
    popup: u64,
    child: u64,
    thread: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || unsafe {
            let class = windows_sys::w!("WinBridgeTargetingFixture");
            let mut wc: WNDCLASSW = mem::zeroed();
            wc.lpszClassName = class;
            wc.lpfnWndProc = Some(proc);
            assert_ne!(RegisterClassW(&wc), 0);
            let root = CreateWindowExW(
                0,
                class,
                windows_sys::w!("Synthetic main"),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                500,
                360,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            let popup = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                windows_sys::w!("Synthetic owned popup"),
                WS_POPUP | WS_BORDER,
                210,
                210,
                180,
                120,
                root,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            let child = CreateWindowExW(
                0,
                class,
                windows_sys::w!("Synthetic child control"),
                WS_CHILD | WS_VISIBLE,
                20,
                30,
                70,
                40,
                root,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            assert!(!root.is_null() && !popup.is_null() && !child.is_null());
            ShowWindow(root, SW_SHOW);
            ShowWindow(popup, SW_SHOW);
            SetForegroundWindow(popup);
            tx.send((
                root as usize as u64,
                popup as usize as u64,
                child as usize as u64,
            ))
            .unwrap();
            let mut msg = mem::zeroed();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            DestroyWindow(popup);
            DestroyWindow(root);
            UnregisterClassW(class, ptr::null_mut());
        });
        let (root, popup, child) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            root,
            popup,
            child,
            thread: Some(thread),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.root as usize as HWND, WM_APP + 9, 0, 0);
        }
        let _ = self.thread.take().unwrap().join();
    }
}

#[tokio::test]
#[ignore = "moves the cursor only over isolated test windows; requires an interactive test desktop"]
async fn same_process_owned_popup_cannot_receive_main_window_clicks() {
    win32::init_dpi();
    let fixture = Fixture::new();
    let injector = Injector::new().unwrap();
    let main = win32::info(fixture.root).unwrap();
    let popup = win32::info(fixture.popup).unwrap();
    assert_eq!(main.pid, popup.pid);
    let mut original: POINT = unsafe { mem::zeroed() };
    unsafe {
        GetCursorPos(&mut original);
    }
    let point = |x: i32, y: i32, kind| {
        Payload::MouseEvent(MouseEvent {
            window_id: main.window_id,
            kind,
            button: 1,
            norm_x: (x - main.x) as f32 / main.width as f32,
            norm_y: (y - main.y) as f32 / main.height as f32,
            wheel_delta_y: 120.,
            ..Default::default()
        })
    };
    let x = popup.x + popup.width as i32 / 2;
    let y = popup.y + popup.height as i32 / 2;
    INPUTS.store(0, Ordering::SeqCst);
    for kind in [2, 4] {
        let error = injector
            .execute(&main, point(x, y, kind))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("TARGET_OBSCURED blocker={}", fixture.popup)
        );
    }
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        INPUTS.load(Ordering::SeqCst),
        0,
        "hidden-on-client popup must receive no click/wheel"
    );
    let mut cursor: POINT = unsafe { mem::zeroed() };
    unsafe {
        GetCursorPos(&mut cursor);
    }
    assert_eq!(
        (cursor.x, cursor.y),
        (original.x, original.y),
        "rejected input must not warp"
    );
    unsafe {
        ShowWindow(fixture.popup as usize as HWND, SW_HIDE);
    }
    let mut rect = unsafe { mem::zeroed() };
    unsafe {
        GetWindowRect(fixture.child as usize as HWND, &mut rect);
    }
    let (cx, cy) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
    injector.execute(&main, point(cx, cy, 2)).await.unwrap();
    injector.execute(&main, point(cx, cy, 3)).await.unwrap();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        INPUTS.load(Ordering::SeqCst),
        1,
        "genuine child controls must remain clickable"
    );
    // Verify the existing ResizeWindow path really changes the Windows surface,
    // independently of Mac native-event capture and Retina point conversion.
    let size = win_host::proto::ResizeWindow {
        window_id: main.window_id, width: main.width + 80, height: main.height + 40, resize_seq: 17,
    };
    let ack = injector.execute(&main, Payload::ResizeWindow(size.clone())).await.unwrap().unwrap();
    assert_eq!(ack.ack_resize_seq, 17);
    thread::sleep(Duration::from_millis(100));
    let resized = win32::info(main.window_id).unwrap();
    assert_eq!((resized.width, resized.height), (size.width, size.height));
    unsafe {
        SetCursorPos(original.x, original.y);
    }
}
