use anyhow::{ensure, Result};
use std::{mem, ptr, slice};
use windows_sys::Win32::{Foundation::HWND, Graphics::Gdi::*, UI::WindowsAndMessaging::*};

/// Draw on black and white to recover alpha for both legacy AND masks and ARGB icons.
fn rgba(icon: HICON, w: u32, h: u32) -> Result<Vec<u8>> {
    ensure!(
        !icon.is_null() && (1..=256).contains(&w) && (1..=256).contains(&h),
        "invalid icon"
    );
    unsafe {
        let dc = CreateCompatibleDC(ptr::null_mut());
        ensure!(!dc.is_null(), "CreateCompatibleDC failed");
        let mut bmi: BITMAPINFO = mem::zeroed();
        bmi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        let mut bits = ptr::null_mut();
        let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        if bmp.is_null() {
            DeleteDC(dc);
            anyhow::bail!("CreateDIBSection failed");
        }
        let old = SelectObject(dc, bmp);
        let len = w as usize * h as usize * 4;
        let render = |background: u8| -> Result<Vec<u8>> {
            ptr::write_bytes(bits.cast::<u8>(), background, len);
            ensure!(
                DrawIconEx(
                    dc,
                    0,
                    0,
                    icon,
                    w as i32,
                    h as i32,
                    0,
                    ptr::null_mut(),
                    DI_NORMAL
                ) != 0,
                "DrawIconEx failed"
            );
            GdiFlush();
            Ok(slice::from_raw_parts(bits.cast::<u8>(), len).to_vec())
        };
        let result = (|| {
            let mut black = render(0)?;
            let white = render(255)?;
            for (b, w) in black
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(white.as_chunks::<4>().0)
            {
                let alpha = 255 - w[0].saturating_sub(b[0]);
                b.swap(0, 2);
                if alpha > 0 {
                    for c in &mut b[..3] {
                        *c = ((u32::from(*c) * 255 / u32::from(alpha)).min(255)) as u8;
                    }
                }
                b[3] = alpha;
            }
            Ok(black)
        })();
        SelectObject(dc, old);
        DeleteObject(bmp);
        DeleteDC(dc);
        result
    }
}

pub(super) fn png(data: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut encoder = png::Encoder::new(&mut result, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(data)?;
    Ok(result)
}
pub(super) fn app_icon(hwnd: HWND) -> Option<Vec<u8>> {
    unsafe {
        let mut icon = GetClassLongPtrW(hwnd, GCLP_HICON);
        if icon == 0 {
            SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                ICON_BIG as usize,
                0,
                SMTO_ABORTIFHUNG,
                50,
                &mut icon,
            );
        }
        if icon == 0 {
            icon = GetClassLongPtrW(hwnd, GCLP_HICONSM);
        }
        if icon == 0 {
            return None;
        }
        png(&rgba(icon as HICON, 32, 32).ok()?, 32, 32).ok()
    }
}
pub(super) fn cursor_pixels(icon: HICON) -> Option<(Vec<u8>, u32, u32, u32, u32)> {
    unsafe {
        let mut info: ICONINFO = mem::zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return None;
        }
        let mut bmp: BITMAP = mem::zeroed();
        let has_color = !info.hbmColor.is_null();
        let handle = if has_color {
            info.hbmColor
        } else {
            info.hbmMask
        };
        let ok = GetObjectW(
            handle,
            mem::size_of::<BITMAP>() as i32,
            (&mut bmp as *mut BITMAP).cast(),
        );
        if has_color {
            DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }
        if ok == 0 {
            return None;
        }
        let w = u32::try_from(bmp.bmWidth).ok()?;
        let h = u32::try_from(bmp.bmHeight / (if has_color { 1 } else { 2 })).ok()?;
        Some((rgba(icon, w, h).ok()?, w, h, info.xHotspot, info.yHotspot))
    }
}
