//! Lightweight Windows System Tray icon for WinBridge Host (`Shell_NotifyIconW`).
//! Displays live connection status tooltip and provides one-click "Copy Pairing Code" and "Open Logs".

use crate::{net::host_info, security};
use std::{
    ffi::c_void,
    mem, ptr,
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Debug)]
pub struct TrayContext {
    pub bind_ip: std::net::IpAddr,
    pub port: u16,
    pub token: String,
    pub fingerprint: Option<String>,
    pub use_tls: bool,
    pub allow_exit: bool,
    pub logs_dir: std::path::PathBuf,
}

#[derive(Default)]
struct TrayShared {
    status_line: String,
    context: Option<TrayContext>,
    hwnd_raw: usize,
}

static SHARED: OnceLock<Mutex<TrayShared>> = OnceLock::new();

fn shared() -> &'static Mutex<TrayShared> {
    SHARED.get_or_init(|| Mutex::new(TrayShared::default()))
}

/// Updates the tray tooltip and menu status header (safe to call from any thread).
pub fn set_status(status: impl Into<String>) {
    let text = status.into();
    let hwnd_raw = {
        let mut guard = shared().lock().unwrap_or_else(|e| e.into_inner());
        guard.status_line = text;
        guard.hwnd_raw
    };
    #[cfg(windows)]
    if hwnd_raw != 0 {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                hwnd_raw as windows_sys::Win32::Foundation::HWND,
                WM_TRAY_UPDATE_STATUS,
                0,
                0,
            );
        }
    }
}

#[cfg(windows)]
const WM_TRAY_CALLBACK: u32 = 0x8000 + 1; // WM_APP + 1
#[cfg(windows)]
const WM_TRAY_UPDATE_STATUS: u32 = 0x8000 + 2;
#[cfg(windows)]
const ID_TRAY_ICON: u32 = 1;
#[cfg(windows)]
const CMD_COPY_PAIRING: usize = 1001;
#[cfg(windows)]
const CMD_OPEN_LOGS: usize = 1002;
#[cfg(windows)]
const CMD_EXIT: usize = 1003;

#[cfg(windows)]
static TASKBAR_CREATED_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
fn copy_wide_into<const N: usize>(dst: &mut [u16; N], text: &str) {
    dst.fill(0);
    for (i, unit) in text.encode_utf16().take(N.saturating_sub(1)).enumerate() {
        dst[i] = unit;
    }
}

#[cfg(windows)]
unsafe fn copy_text_to_clipboard(hwnd: windows_sys::Win32::Foundation::HWND, text: &str) -> bool {
    use windows_sys::Win32::System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    };
    const CF_UNICODETEXT: u32 = 13;

    let wide = to_wide(text);
    let bytes = wide.len() * mem::size_of::<u16>();
    let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
    if hmem.is_null() {
        return false;
    }
    let ptr_dst = GlobalLock(hmem) as *mut u16;
    if ptr_dst.is_null() {
        GlobalFree(hmem);
        return false;
    }
    ptr::copy_nonoverlapping(wide.as_ptr(), ptr_dst, wide.len());
    GlobalUnlock(hmem);

    if OpenClipboard(hwnd) == 0 {
        GlobalFree(hmem);
        return false;
    }
    let _ = EmptyClipboard();
    let ok = !SetClipboardData(CF_UNICODETEXT, hmem).is_null();
    let _ = CloseClipboard();
    if !ok {
        GlobalFree(hmem);
    }
    ok
}

#[cfg(windows)]
fn build_live_pairing_uri(ctx: &TrayContext) -> String {
    let preferred_ip = if ctx.bind_ip.is_unspecified() {
        None
    } else {
        Some(ctx.bind_ip)
    };
    let (lan_ip, mac) = host_info::detect_lan_ipv4_and_mac(preferred_ip);
    let host_str = lan_ip.map(|ip| ip.to_string()).unwrap_or_else(|| {
        if ctx.bind_ip.is_unspecified() {
            "127.0.0.1".to_owned()
        } else {
            ctx.bind_ip.to_string()
        }
    });
    let host_name = std::env::var("COMPUTERNAME").ok();
    host_info::build_pairing_uri(
        &host_str,
        ctx.port,
        &ctx.token,
        ctx.fingerprint.as_deref(),
        mac.as_deref(),
        host_name.as_deref(),
        ctx.use_tls,
    )
}

#[cfg(windows)]
unsafe fn load_tray_icon() -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            LoadIconW, LoadImageW, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED,
        },
    };
    let hinst = GetModuleHandleW(ptr::null());
    let custom = LoadImageW(
        hinst,
        1usize as *const u16,
        IMAGE_ICON,
        0,
        0,
        LR_DEFAULTSIZE | LR_SHARED,
    );
    if !custom.is_null() {
        return custom as _;
    }
    LoadIconW(ptr::null_mut(), IDI_APPLICATION)
}

#[cfg(windows)]
unsafe fn apply_tray_icon(
    hwnd: windows_sys::Win32::Foundation::HWND,
    action: u32,
    tip: &str,
    balloon: Option<(&str, &str)>,
) {
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NOTIFYICONDATAW,
    };
    let mut nid: NOTIFYICONDATAW = mem::zeroed();
    nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = ID_TRAY_ICON;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY_CALLBACK;
    nid.hIcon = load_tray_icon();
    copy_wide_into(&mut nid.szTip, tip);
    if let Some((title, body)) = balloon {
        nid.uFlags |= NIF_INFO;
        copy_wide_into(&mut nid.szInfoTitle, title);
        copy_wide_into(&mut nid.szInfo, body);
        nid.dwInfoFlags = NIIF_INFO;
    }
    let _ = Shell_NotifyIconW(action, &nid);
}

#[cfg(windows)]
unsafe fn show_context_menu(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::{
        Foundation::POINT,
        UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW,
            SetForegroundWindow, TrackPopupMenu, MF_GRAYED, MF_SEPARATOR, MF_STRING,
            TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, WM_NULL,
        },
    };
    let (status_line, ctx) = {
        let g = shared().lock().unwrap_or_else(|e| e.into_inner());
        (g.status_line.clone(), g.context.clone())
    };
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let header = if status_line.is_empty() {
        "WinBridge Host 运行中".to_owned()
    } else {
        status_line
    };
    let header_w = to_wide(&header);
    let copy_w = to_wide("复制配对码 (winbridge://...)");
    let logs_w = to_wide("打开日志目录");
    let exit_w = to_wide("退出 WinBridge");

    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, header_w.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, CMD_COPY_PAIRING, copy_w.as_ptr());
    AppendMenuW(menu, MF_STRING, CMD_OPEN_LOGS, logs_w.as_ptr());
    if ctx.as_ref().is_some_and(|c| c.allow_exit) {
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, CMD_EXIT, exit_w.as_ptr());
    }

    let mut pt: POINT = mem::zeroed();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
        pt.x,
        pt.y,
        0,
        hwnd,
        ptr::null(),
    ) as usize;
    let _ = PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);

    match cmd {
        CMD_COPY_PAIRING => {
            if let Some(ctx) = ctx {
                let uri = build_live_pairing_uri(&ctx);
                if copy_text_to_clipboard(hwnd, &uri) {
                    apply_tray_icon(
                        hwnd,
                        windows_sys::Win32::UI::Shell::NIM_MODIFY,
                        &header,
                        Some(("WinBridge 配对码已复制", "在 Mac 客户端点击「粘贴配对码」即可一键导入。")),
                    );
                }
            }
        }
        CMD_OPEN_LOGS => {
            let dir = ctx
                .map(|c| c.logs_dir)
                .unwrap_or_else(security::program_data_dir);
            let _ = std::fs::create_dir_all(&dir);
            let verb = to_wide("open");
            let path_w = to_wide(&dir.to_string_lossy());
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                ptr::null_mut(),
                verb.as_ptr(),
                path_w.as_ptr(),
                ptr::null(),
                ptr::null(),
                windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            );
        }
        CMD_EXIT => {
            std::process::exit(0);
        }
        _ => {}
    }
}

#[cfg(windows)]
unsafe extern "system" fn tray_wnd_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::{
        Shell::{NIM_ADD, NIM_MODIFY},
        WindowsAndMessaging::{DefWindowProcW, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_RBUTTONUP},
    };
    let taskbar_msg = TASKBAR_CREATED_MSG.load(std::sync::atomic::Ordering::Relaxed);
    if taskbar_msg != 0 && msg == taskbar_msg {
        let tip = shared()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status_line
            .clone();
        apply_tray_icon(hwnd, NIM_ADD, &tip, None);
        return 0;
    }
    match msg {
        WM_TRAY_CALLBACK => {
            let event = (lparam & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_CONTEXTMENU || event == WM_LBUTTONDBLCLK {
                show_context_menu(hwnd);
            }
            0
        }
        WM_TRAY_UPDATE_STATUS => {
            let tip = shared()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status_line
                .clone();
            apply_tray_icon(hwnd, NIM_MODIFY, &tip, None);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Spawns a background daemon thread displaying the WinBridge system tray icon.
/// Silently no-ops on non-interactive desktops (e.g. Lock Screen) until Explorer starts.
pub fn spawn(ctx: TrayContext) {
    let initial_status = format!("WinBridge — 监听中 :{}", ctx.port);
    {
        let mut guard = shared().lock().unwrap_or_else(|e| e.into_inner());
        guard.status_line = initial_status.clone();
        guard.context = Some(ctx);
    }

    #[cfg(windows)]
    {
        let _ = std::thread::Builder::new()
            .name("winbridge-tray".into())
            .spawn(move || unsafe {
                use windows_sys::Win32::{
                    System::LibraryLoader::GetModuleHandleW,
                    UI::{
                        Shell::{Shell_NotifyIconW, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW},
                        WindowsAndMessaging::{
                            CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassW,
                            RegisterWindowMessageW, TranslateMessage, MSG, WNDCLASSW,
                        },
                    },
                };

                let hinst = GetModuleHandleW(ptr::null());
                let class_name = to_wide("WinBridgeTrayWindow");
                let mut wc: WNDCLASSW = mem::zeroed();
                wc.lpfnWndProc = Some(tray_wnd_proc);
                wc.hInstance = hinst;
                wc.lpszClassName = class_name.as_ptr();
                let _ = RegisterClassW(&wc);

                let tb_msg = RegisterWindowMessageW(to_wide("TaskbarCreated").as_ptr());
                TASKBAR_CREATED_MSG.store(tb_msg, std::sync::atomic::Ordering::Relaxed);

                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    class_name.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    hinst,
                    ptr::null::<c_void>(),
                );
                if hwnd.is_null() {
                    return;
                }
                {
                    let mut guard = shared().lock().unwrap_or_else(|e| e.into_inner());
                    guard.hwnd_raw = hwnd as usize;
                }
                apply_tray_icon(hwnd, NIM_ADD, &initial_status, None);

                let mut msg: MSG = mem::zeroed();
                while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                let mut nid: NOTIFYICONDATAW = mem::zeroed();
                nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = hwnd;
                nid.uID = ID_TRAY_ICON;
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            });
    }
}
