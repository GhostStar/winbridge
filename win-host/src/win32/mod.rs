mod clipboard;
mod cursor;
mod event_hook;
pub use event_hook::{Event, Observer};
mod image;
mod window;
pub use window::{candidates, foreground_id, info, list};
pub(crate) use window::{checked_hwnd, resize};

pub fn init_dpi() {
    use windows_sys::Win32::UI::HiDpi::*;
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
