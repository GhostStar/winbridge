use super::d3d11::Graphics;
use anyhow::{bail, ensure, Context, Result};
use std::{
    mem::size_of,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{factory, Interface},
    Foundation::{EventRegistrationToken, TypedEventHandler},
    Graphics::{Capture::*, DirectX::DirectXPixelFormat, SizeInt32},
    Win32::{
        Foundation::{BOOL, HWND, LPARAM, RECT},
        Graphics::{
            Direct3D11::{ID3D11Texture2D, D3D11_BOX, D3D11_TEXTURE2D_DESC},
            Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
            Gdi::{
                EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR,
                MONITORINFO, MONITOR_DEFAULTTONEAREST,
            },
        },
        System::WinRT::{
            Direct3D11::IDirect3DDxgiInterfaceAccess,
            Graphics::Capture::IGraphicsCaptureItemInterop, RoInitialize, RoUninitialize,
            RO_INIT_MULTITHREADED,
        },
        UI::WindowsAndMessaging::{
            GetSystemMetrics, GetWindowRect, IsIconic, IsWindow, SM_CMONITORS,
        },
    },
};

pub(super) struct Apartment;
impl Apartment {
    pub fn new() -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.context("initialize WinRT MTA")?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

pub(super) struct Capture {
    pub gpu: Graphics,
    pub frame_start: Instant,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    size: SizeInt32,
    frame: Option<Direct3D11CaptureFrame>,
    hwnd: HWND,
    monitor: Option<HMONITOR>,
    arrival: Receiver<()>,
    token: EventRegistrationToken,
    item: GraphicsCaptureItem,
    closed_token: EventRegistrationToken,
    closed: Arc<AtomicBool>,
}

pub(super) fn minimized(id: u64) -> bool {
    unsafe { IsIconic(HWND(id as usize as _)).as_bool() }
}

pub(super) fn check_desktop(id: u64) -> Result<()> {
    ensure!(
        unsafe { IsWindow(HWND(id as usize as _)) }.as_bool(),
        "WINDOW_NOT_FOUND: 0x{id:X}"
    );
    ensure!(
        unsafe { GetSystemMetrics(SM_CMONITORS) } > 0,
        "DISPLAY_DISCONNECTED: Windows reports no active monitors; check the display connection"
    );
    Ok(())
}

fn check_monitors() -> Result<()> {
    unsafe extern "system" fn found(_: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        *(data.0 as *mut bool) = true;
        BOOL(1)
    }
    let mut present = false;
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(found),
            LPARAM((&mut present as *mut bool) as isize),
        )
        .ok()?
    };
    ensure!(
        present,
        "DISPLAY_DISCONNECTED: no monitors are attached to the interactive desktop"
    );
    Ok(())
}

fn monitor_rect(monitor: HMONITOR) -> Result<RECT> {
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe { GetMonitorInfoW(monitor, &mut mi) }.as_bool(),
        "DISPLAY_DISCONNECTED: GetMonitorInfoW failed"
    );
    Ok(mi.rcMonitor)
}

// ContentSize can already exclude the invisible DWM border on some versions of Windows.
// Calculate the origin for each axis separately and never read the stale area of a pooled texture.
fn crop_axis(content: u32, extent: i32, origin_delta: i32) -> (u32, u32) {
    let visible = extent.max(0) as u32;
    let origin = (origin_delta.max(0) as u32).min(content.saturating_sub(visible));
    (origin, visible.min(content - origin))
}

fn crop_monitor_axis(tex_extent: u32, mon_origin: i32, win_start: i32, win_end: i32) -> (u32, u32) {
    let max = tex_extent.min(i32::MAX as u32) as i32;
    let start = win_start.saturating_sub(mon_origin).clamp(0, max);
    let end = win_end.saturating_sub(mon_origin).clamp(start, max);
    (start as u32, (end - start) as u32)
}

impl Capture {
    pub fn new(id: u64, cursor: bool, monitor_crop: bool) -> Result<Self> {
        check_desktop(id)?;
        check_monitors()?;
        ensure!(
            GraphicsCaptureSession::IsSupported()?,
            "Windows Graphics Capture is unavailable on this desktop"
        );
        let hwnd = HWND(id as usize as _);
        let gpu = Graphics::new()?;
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let (item, monitor): (GraphicsCaptureItem, Option<HMONITOR>) = if monitor_crop {
            let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
            ensure!(
                !hmon.is_invalid(),
                "DISPLAY_DISCONNECTED: window is not located on any active monitor"
            );
            let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(hmon) }?;
            (item, Some(hmon))
        } else {
            let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd) }?;
            (item, None)
        };
        let size = item.Size()?;
        ensure!(size.Width > 0 && size.Height > 0, "empty WGC frame size");
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&gpu.runtime, FORMAT, 2, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        // On newer Windows versions let the compositor deliver updates at its own rate.
        // The capture worker applies the negotiated per-window FPS cap.
        let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan { Duration: 0 });
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        let (tx, arrival) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let closed_handler = closed.clone();
        let closed_tx = tx.clone();
        let closed_token = item.Closed(&TypedEventHandler::new(move |_, _| {
            closed_handler.store(true, Ordering::Release);
            let _ = closed_tx.try_send(());
            Ok(())
        }))?;
        let token = pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            let _ = tx.try_send(());
            Ok(())
        }))?;
        let capture = Self {
            gpu,
            pool,
            session,
            size,
            hwnd,
            monitor,
            arrival,
            token,
            item,
            closed_token,
            closed,
            frame: None,
            frame_start: Instant::now(),
        };
        capture.session.StartCapture()?;
        Ok(capture)
    }

    pub fn monitor_changed(&self) -> bool {
        let Some(current) = self.monitor else {
            return false;
        };
        let target = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        !target.is_invalid() && target != current
    }

    pub fn next(&mut self) -> Result<Option<(ID3D11Texture2D, D3D11_BOX)>> {
        match self.arrival.recv_timeout(Duration::from_millis(80)) {
            Ok(()) => (),
            Err(RecvTimeoutError::Timeout) => {
                unsafe { self.gpu.device.GetDeviceRemovedReason() }
                    .context("check capture device after idle frame pool")?;
                return Ok(None);
            }
            Err(RecvTimeoutError::Disconnected) => bail!("WGC frame pool closed"),
        }
        if self.closed.load(Ordering::Acquire) {
            if self.monitor.is_some() {
                bail!("DISPLAY_DISCONNECTED: monitor capture item closed");
            }
            bail!("WINDOW_NOT_FOUND: capture item closed");
        }
        self.frame_start = Instant::now();
        let mut frame = match self.pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(e) if e.code().is_ok() || e.code() == windows::Win32::Foundation::E_POINTER => {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        };
        // Arrival notifications are coalesced while an encoder is busy. Consume the
        // second buffer as well so the last update of a now static window is delivered.
        match self.pool.TryGetNextFrame() {
            Ok(newer) => {
                frame.Close()?;
                frame = newer;
            }
            Err(e) if e.code().is_ok() || e.code() == windows::Win32::Foundation::E_POINTER => (),
            Err(e) => return Err(e.into()),
        }
        let size = frame.ContentSize()?;
        if size.Width <= 0 || size.Height <= 0 {
            frame.Close()?;
            return Ok(None);
        }
        if size != self.size {
            frame.Close()?;
            self.pool.Recreate(&self.gpu.runtime, FORMAT, 2, size)?;
            self.size = size;
            return Ok(None);
        }
        let interop: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let surface: ID3D11Texture2D = unsafe { interop.GetInterface() }?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { surface.GetDesc(&mut desc) };
        let mut outer = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut outer)? };
        let mut ext = RECT::default();
        let got_ext = unsafe {
            DwmGetWindowAttribute(
                self.hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut ext as *mut RECT).cast(),
                size_of::<RECT>() as u32,
            )
        };
        if got_ext.is_err() {
            ext = outer;
        }
        let (x, w, y, h) = if let Some(mon) = self.monitor {
            let mon_rect = monitor_rect(mon)?;
            let (x, w) = crop_monitor_axis(
                desc.Width.min(size.Width as u32),
                mon_rect.left,
                ext.left,
                ext.right,
            );
            let (y, h) = crop_monitor_axis(
                desc.Height.min(size.Height as u32),
                mon_rect.top,
                ext.top,
                ext.bottom,
            );
            (x, w, y, h)
        } else {
            let (x, w) = crop_axis(
                desc.Width.min(size.Width as u32),
                ext.right - ext.left,
                ext.left - outer.left,
            );
            let (y, h) = crop_axis(
                desc.Height.min(size.Height as u32),
                ext.bottom - ext.top,
                ext.top - outer.top,
            );
            (x, w, y, h)
        };
        if w == 0 || h == 0 {
            frame.Close()?;
            return Ok(None);
        }
        // Copy before the frame is returned to WGC. Holding just its ID3D11Texture2D
        // reference does not stop the pool from overwriting that texture.
        let area = D3D11_BOX {
            left: x,
            top: y,
            front: 0,
            right: x + w,
            bottom: y + h,
            back: 1,
        };
        self.frame = Some(frame);
        Ok(Some((surface, area)))
    }

    pub fn release(&mut self) -> Result<()> {
        if let Some(frame) = self.frame.take() {
            frame.Close()?;
        }
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.release();
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

#[cfg(test)]
mod tests {
    use super::{crop_axis, crop_monitor_axis};
    #[test]
    fn handles_both_capture_bounds() {
        assert_eq!(crop_axis(1014, 1000, 7), (7, 1000));
        assert_eq!(crop_axis(1000, 1000, 7), (0, 1000));
        assert_eq!(crop_axis(980, 1000, 7), (0, 980));
    }
    #[test]
    fn crops_monitor_bounds_for_window() {
        assert_eq!(crop_monitor_axis(1920, 0, 100, 1100), (100, 1000));
        // Secondary monitor with negative origin and partially off-screen window
        assert_eq!(crop_monitor_axis(1920, -1920, -2000, -1000), (0, 920));
        assert_eq!(crop_monitor_axis(1920, 0, 1800, 2200), (1800, 120));
    }
}
