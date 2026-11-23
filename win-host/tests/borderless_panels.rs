//! Native style classification using only synthetic windows.
use std::ptr;
use win_host::win32;
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
};

struct TestWindow(HWND);
impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}
impl TestWindow {
    unsafe fn new(owner: HWND, style: u32, ex: u32) -> Self {
        let hwnd = CreateWindowExW(
            ex,
            windows_sys::w!("STATIC"),
            windows_sys::w!("WinBridge synthetic panel"),
            style,
            100,
            100,
            240,
            180,
            owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        assert!(!hwnd.is_null());
        // Guest tools can launch the test with STARTF_USESHOWWINDOW/SW_HIDE.
        // The first call follows that startup flag; the second explicitly shows
        // only our synthetic fixture without activating another application.
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Self(hwnd)
    }
    #[track_caller]
    fn info(&self) -> win_host::proto::WindowInfo {
        win32::info(self.0 as usize as u64).unwrap()
    }
}

#[test]
#[ignore = "creates isolated native windows; needs an unlocked test desktop"]
fn native_cards_keep_focusable_role_without_extra_chrome() {
    unsafe {
        let root = TestWindow::new(ptr::null_mut(), WS_OVERLAPPEDWINDOW, 0);
        let card = TestWindow::new(root.0, WS_POPUP, WS_EX_TOOLWINDOW);
        let dialog = TestWindow::new(root.0, WS_POPUP | WS_CAPTION, WS_EX_TOOLWINDOW);
        let image = TestWindow::new(ptr::null_mut(), WS_POPUP, 0);
        let tool = TestWindow::new(root.0, WS_POPUP | WS_THICKFRAME, WS_EX_TOOLWINDOW);
        assert_eq!(card.info().kind, 2); // focusable panel, NOT a nonactivating menu
        assert!(card.info().is_borderless_panel);
        assert!(!dialog.info().is_borderless_panel);
        assert!(!tool.info().is_borderless_panel);
        assert_eq!(image.info().kind, 1);
        assert!(!image.info().is_borderless_panel);
        EnableWindow(root.0, 0);
        let modal = card.info();
        assert!(modal.is_modal);
        assert!(!modal.is_borderless_panel);
        EnableWindow(root.0, 1);
        // Same HWND can change native style without closing/reopening.
        SetWindowLongW(
            card.0,
            GWL_STYLE,
            (WS_POPUP | WS_CAPTION | WS_VISIBLE) as i32,
        );
        assert!(!card.info().is_borderless_panel);
        SetWindowLongW(card.0, GWL_STYLE, (WS_POPUP | WS_VISIBLE) as i32);
        assert!(card.info().is_borderless_panel);
    }
}
