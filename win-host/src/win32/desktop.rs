use anyhow::{bail, ensure, Result};
use std::{
    cell::Cell,
    io, mem, ptr, slice,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, HANDLE},
    Graphics::Gdi::*,
    Security::{GetTokenInformation, TokenUIAccess, TOKEN_QUERY},
    System::{
        RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken},
        StationsAndDesktops::{
            CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop,
            SetThreadDesktop, HDESK, UOI_NAME,
        },
        Threading::{GetCurrentProcess, GetCurrentThreadId, OpenProcessToken},
    },
    UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN},
};

const DESKTOP_ACCESS_ALL: u32 = 0x01FF;
const DESKTOP_READ_ONLY: u32 = 0x0001;

static SECURE_DESKTOP_HINT: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopState {
    Default,
    /// Active input desktop is a non-Default desktop (e.g. Winlogon UAC or Lock Screen)
    /// and this process (or its LocalSystem secure-helper) has permission to access it.
    SecureAccessible,
    /// Active input desktop is Winlogon / Secure Desktop, but current process lacks
    /// SYSTEM/TCB permission to open it.
    SecureDenied,
}

pub fn set_secure_desktop_hint(active: bool) {
    SECURE_DESKTOP_HINT.store(active, Ordering::Release);
}

pub fn current_process_ui_access() -> Option<bool> {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut ui_access: u32 = 0;
        let mut len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenUIAccess,
            (&mut ui_access as *mut u32).cast(),
            mem::size_of::<u32>() as u32,
            &mut len,
        );
        CloseHandle(token);
        (ok != 0).then_some(ui_access != 0)
    }
}

fn desktop_name(desk: HDESK) -> Option<String> {
    if desk.is_null() {
        return None;
    }
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    let ok = unsafe {
        GetUserObjectInformationW(
            desk as HANDLE,
            UOI_NAME,
            buf.as_mut_ptr().cast(),
            (buf.len() * mem::size_of::<u16>()) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Probes the current process's own desktop access directly via `OpenInputDesktop`.
pub fn probe_desktop_state_local() -> DesktopState {
    unsafe {
        let desk = OpenInputDesktop(0, 0, DESKTOP_READ_ONLY);
        if desk.is_null() {
            let err = GetLastError();
            if err == ERROR_ACCESS_DENIED {
                return DesktopState::SecureDenied;
            }
            return DesktopState::Default;
        }
        let name = desktop_name(desk);
        CloseDesktop(desk);
        match name.as_deref() {
            Some(n) if n.eq_ignore_ascii_case("Default") => DesktopState::Default,
            Some(_) => DesktopState::SecureAccessible,
            None => DesktopState::Default,
        }
    }
}

/// Probes the active input desktop, delegating to the LocalSystem `secure-helper` when running
/// as an interactive user session worker under the Windows Service.
pub fn probe_desktop_state() -> DesktopState {
    if let Some(state) = crate::helper::probe_desktop_state() {
        return state;
    }
    probe_desktop_state_local()
}

pub fn is_secure_desktop() -> bool {
    if crate::helper::is_configured() {
        return SECURE_DESKTOP_HINT.load(Ordering::Acquire);
    }
    probe_desktop_state_local() != DesktopState::Default
}

thread_local! {
    static OWNED_DESKTOP: Cell<HDESK> = const { Cell::new(ptr::null_mut()) };
}

/// Restores the calling thread's original desktop on drop so the last `OpenInputDesktop` handle
/// can be closed without leaking kernel handles.
pub struct ThreadDesktopResetGuard {
    orig: HDESK,
}

impl ThreadDesktopResetGuard {
    pub fn new() -> Self {
        Self {
            orig: unsafe { GetThreadDesktop(GetCurrentThreadId()) },
        }
    }
}

impl Default for ThreadDesktopResetGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ThreadDesktopResetGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.orig.is_null() {
                let _ = SetThreadDesktop(self.orig);
            }
            let prev = OWNED_DESKTOP.with(|slot| slot.replace(ptr::null_mut()));
            if !prev.is_null() {
                CloseDesktop(prev);
            }
        }
    }
}

/// Switches the calling thread to whichever interactive desktop currently owns user input
/// (`Default` vs `Winlogon` UAC / Lock Screen).
///
/// NOTE: The calling thread MUST NOT own any Win32 windows or `SetWinEventHook` hooks.
/// Returns `Ok(true)` if now attached to a Secure Desktop (`Winlogon`), `Ok(false)` if on `Default`.
pub fn sync_thread_desktop() -> Result<bool> {
    unsafe {
        let input = OpenInputDesktop(0, 0, DESKTOP_ACCESS_ALL);
        if input.is_null() {
            let err = GetLastError();
            if err == ERROR_ACCESS_DENIED {
                bail!(
                    "ACCESS_DENIED_ELEVATED: UAC 安全桌面已拦截（请以管理员运行 win-host --install-service 安装为系统服务）"
                );
            }
            return Ok(false);
        }
        let target_name = desktop_name(input).unwrap_or_default();
        let current = GetThreadDesktop(GetCurrentThreadId());
        let current_name = desktop_name(current).unwrap_or_default();

        if !target_name.is_empty() && target_name.eq_ignore_ascii_case(&current_name) {
            CloseDesktop(input);
            return Ok(!target_name.eq_ignore_ascii_case("Default"));
        }

        if SetThreadDesktop(input) == 0 {
            let err = io::Error::last_os_error();
            CloseDesktop(input);
            bail!("SetThreadDesktop({target_name}) failed: {err}");
        }

        let prev = OWNED_DESKTOP.with(|slot| slot.replace(input));
        if !prev.is_null() {
            CloseDesktop(prev);
        }
        Ok(!target_name.eq_ignore_ascii_case("Default"))
    }
}

/// Returns the primary user token of the active interactive console session when running as
/// `LocalSystem` (requires `SE_TCB_NAME`). Returns `None` in regular non-service user mode.
pub fn active_console_user_token() -> Option<HANDLE> {
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            return None;
        }
        let mut token: HANDLE = ptr::null_mut();
        if WTSQueryUserToken(session_id, &mut token) != 0 && !token.is_null() {
            Some(token)
        } else {
            None
        }
    }
}

/// Captures the primary display of the calling thread's currently bound desktop (`Default` or
/// `Winlogon` Secure Desktop) into a tightly packed top-down 32-bit BGRA buffer.
pub fn capture_gdi_primary_bgra() -> Result<(u16, u16, Vec<u8>)> {
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        ensure!(w > 0 && h > 0, "DISPLAY_DISCONNECTED: empty GDI screen size");
        let width = u16::try_from(w)?;
        let height = u16::try_from(h)?;

        let screen_dc = GetDC(ptr::null_mut());
        ensure!(!screen_dc.is_null(), "GetDC(NULL) failed on active desktop");
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_null() {
            ReleaseDC(ptr::null_mut(), screen_dc);
            bail!("CreateCompatibleDC failed");
        }

        let mut bmi: BITMAPINFO = mem::zeroed();
        bmi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // top-down BGRA
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut bits = ptr::null_mut();
        let bmp = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        if bmp.is_null() || bits.is_null() {
            DeleteDC(mem_dc);
            ReleaseDC(ptr::null_mut(), screen_dc);
            bail!("CreateDIBSection failed");
        }

        let old = SelectObject(mem_dc, bmp);
        let blt_ok = BitBlt(mem_dc, 0, 0, w, h, screen_dc, 0, 0, SRCCOPY) != 0;
        let result = if blt_ok {
            GdiFlush();
            let len = (w as usize) * (h as usize) * 4;
            let mut pixels = slice::from_raw_parts(bits.cast::<u8>().cast_const(), len).to_vec();
            for px in pixels.chunks_exact_mut(4) {
                px[3] = 255;
            }
            Ok((width, height, pixels))
        } else {
            Err(anyhow::anyhow!(
                "BitBlt failed on desktop: {}",
                io::Error::last_os_error()
            ))
        };

        SelectObject(mem_dc, old);
        DeleteObject(bmp);
        DeleteDC(mem_dc);
        ReleaseDC(ptr::null_mut(), screen_dc);
        result
    }
}

/// Captures frames from the active Secure Desktop (`Winlogon`) either via the LocalSystem
/// `secure-helper` loopback IPC (preferred) or locally on a dedicated clean OS thread.
pub struct SecureGdiCapturer {
    stop: Arc<AtomicBool>,
    rx: Option<Receiver<(u16, u16, Vec<u8>)>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SecureGdiCapturer {
    pub fn start() -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (tx, rx) = mpsc::sync_channel::<(u16, u16, Vec<u8>)>(1);
        let use_helper = crate::helper::is_configured();
        let thread = std::thread::Builder::new()
            .name("secure-gdi-capture".into())
            .spawn(move || {
                let _guard = ThreadDesktopResetGuard::new();
                while !worker_stop.load(Ordering::Acquire) {
                    let tick = Instant::now();
                    let frame = if use_helper {
                        crate::helper::capture_secure_frame().ok()
                    } else if sync_thread_desktop().is_ok() {
                        capture_gdi_primary_bgra().ok()
                    } else {
                        None
                    };
                    if let Some(frame) = frame {
                        let _ = tx.try_send(frame);
                    }
                    let elapsed = tick.elapsed();
                    if elapsed < Duration::from_millis(50) {
                        std::thread::sleep(Duration::from_millis(50) - elapsed);
                    }
                }
            })?;
        Ok(Self {
            stop,
            rx: Some(rx),
            thread: Some(thread),
        })
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<(u16, u16, Vec<u8>)> {
        self.rx.as_ref()?.recv_timeout(timeout).ok()
    }
}

impl Drop for SecureGdiCapturer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        drop(self.rx.take());
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}
