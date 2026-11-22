use crate::proto::{clipboard_sync::Content, ClipboardSync};
use anyhow::{bail, ensure, Context, Result};
use std::{io, ptr, slice};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HGLOBAL, HWND},
    System::{DataExchange::*, Memory::*},
};

// Clipboard format IDs from WinUser.h. PNG is registered dynamically by name.
const TEXT: u32 = 13;
const DIB: u32 = 8;
const DIBV5: u32 = 17;
const MAX_WIRE: usize = 8 * 1024 * 1024;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_BITMAP: usize = 64 * 1024 * 1024;
fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(windows_sys::w!("PNG")) }
}

struct Open;
impl Open {
    fn new(hwnd: HWND) -> Result<Self> {
        for _ in 0..5 {
            if unsafe { OpenClipboard(hwnd) } != 0 {
                return Ok(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err(io::Error::last_os_error()).context("OpenClipboard")
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

struct Block(HGLOBAL);
impl Block {
    fn copy(data: &[u8]) -> Result<Self> {
        unsafe {
            let block = Self(GlobalAlloc(GMEM_MOVEABLE, data.len()));
            ensure!(!block.0.is_null(), "GlobalAlloc failed");
            let dest = GlobalLock(block.0);
            ensure!(!dest.is_null(), "GlobalLock failed");
            ptr::copy_nonoverlapping(data.as_ptr(), dest.cast(), data.len());
            GlobalUnlock(block.0);
            Ok(block)
        }
    }
    fn publish(mut self, format: u32) -> Result<()> {
        ensure!(
            !unsafe { SetClipboardData(format, self.0) }.is_null(),
            "SetClipboardData failed"
        );
        self.0 = ptr::null_mut();
        Ok(())
    }
}
impl Drop for Block {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                GlobalFree(self.0);
            }
        }
    }
}

fn contents(format: u32, limit: usize) -> Result<Option<Vec<u8>>> {
    if format == 0 || unsafe { IsClipboardFormatAvailable(format) } == 0 {
        return Ok(None);
    }
    unsafe {
        let h = GetClipboardData(format);
        ensure!(!h.is_null(), "GetClipboardData failed");
        let size = GlobalSize(h);
        ensure!(size <= limit, "clipboard data exceeds supported size");
        let p = GlobalLock(h);
        ensure!(!p.is_null(), "GlobalLock failed");
        let bytes = slice::from_raw_parts(p.cast::<u8>(), size).to_vec();
        GlobalUnlock(h);
        Ok(Some(bytes))
    }
}
fn field(data: &[u8], i: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(i..i + 4).context("short DIB header")?.try_into()?,
    ))
}
fn dib_png(data: &[u8]) -> Result<Vec<u8>> {
    let header = field(data, 0)? as usize;
    ensure!(
        header >= 40 && header <= data.len(),
        "unsupported DIB header"
    );
    let w = field(data, 4)? as i32;
    let signed_h = field(data, 8)? as i32;
    let h = signed_h.unsigned_abs() as usize;
    ensure!(w > 0 && h > 0, "empty clipboard image");
    let width = w as usize;
    ensure!(
        width.checked_mul(h).is_some_and(|n| n <= MAX_BITMAP / 4),
        "oversized clipboard bitmap"
    );
    let depth = u16::from_le_bytes(data[14..16].try_into()?);
    let compression = field(data, 16)?;
    ensure!(
        depth == 24 || depth == 32,
        "unsupported clipboard pixel depth"
    );
    ensure!(
        compression == 0 || compression == 3,
        "unsupported DIB compression"
    );
    let mut offset = header;
    if compression == 3 {
        ensure!(
            depth == 32
                && field(data, 40)? == 0xff0000
                && field(data, 44)? == 0xff00
                && field(data, 48)? == 0xff,
            "unsupported DIB color masks"
        );
        if header == 40 {
            offset += 12;
        }
    }
    let palette = field(data, 32)? as usize;
    offset = offset
        .checked_add(palette.checked_mul(4).context("invalid palette")?)
        .context("invalid DIB offset")?;
    let bytes_per_pixel = depth as usize / 8;
    let stride = (width * bytes_per_pixel + 3) & !3;
    let pixels = data.get(offset..).context("truncated DIB")?;
    ensure!(pixels.len() >= h * stride, "truncated DIB scanlines");
    let alpha = depth == 32 && header >= 56 && field(data, 52)? == 0xff000000;
    let mut rgba = Vec::with_capacity(width * h * 4);
    for y in 0..h {
        let row = if signed_h < 0 { y } else { h - 1 - y };
        for bgra in pixels[row * stride..row * stride + width * bytes_per_pixel]
            .chunks_exact(bytes_per_pixel)
        {
            rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], if alpha { bgra[3] } else { 255 }]);
        }
    }
    super::image::png(&rgba, width as u32, h as u32)
}
fn png_dib(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let (w, h) = reader.info().size();
    ensure!(
        u64::from(w) * u64::from(h) <= MAX_BITMAP as u64 / 4,
        "oversized clipboard PNG"
    );
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels)?;
    let mut dib = vec![0; 124];
    for (index, value) in [
        (0, 124),
        (4, w),
        (8, (-(h as i32)) as u32),
        (12, (32 << 16) | 1),
        (16, 3),
        (40, 0xff0000),
        (44, 0xff00),
        (48, 0xff),
        (52, 0xff000000),
        (56, 0x73524742),
    ] {
        dib[index..index + 4].copy_from_slice(&u32::to_le_bytes(value));
    }
    dib.reserve(w as usize * h as usize * 4);
    for pixel in pixels[..info.buffer_size()].chunks_exact(info.color_type.samples()) {
        let bgra = match info.color_type {
            png::ColorType::Rgba => [pixel[2], pixel[1], pixel[0], pixel[3]],
            png::ColorType::Rgb => [pixel[2], pixel[1], pixel[0], 255],
            png::ColorType::Grayscale => [pixel[0], pixel[0], pixel[0], 255],
            png::ColorType::GrayscaleAlpha => [pixel[0], pixel[0], pixel[0], pixel[1]],
            png::ColorType::Indexed => bail!("unexpected unexpanded palette"),
        };
        dib.extend_from_slice(&bgra);
    }
    Ok(dib)
}

pub(super) fn read(
    hwnd: HWND,
    scope: &super::scoped_clipboard::Scope,
) -> Result<Option<ClipboardSync>> {
    let guard = Open::new(hwnd)?;
    scope.validate()?;
    if !scope.owns_clipboard() || unsafe { IsClipboardFormatAvailable(15) } != 0 {
        return Ok(None);
    }
    let seq = unsafe { GetClipboardSequenceNumber() };
    let image = contents(png_format(), MAX_WIRE)?;
    let bitmap = if image.is_none() {
        match contents(DIBV5, MAX_BITMAP + 124)? {
            Some(bytes) => Some(bytes),
            None => contents(DIB, MAX_BITMAP + 124)?,
        }
    } else {
        None
    };
    let text = if image.is_none() && bitmap.is_none() {
        contents(TEXT, MAX_TEXT * 2 + 2)?
    } else {
        None
    };
    drop(guard);
    let content = if let Some(png) = image {
        Content::ImagePng(png)
    } else if let Some(dib) = bitmap {
        Content::ImagePng(dib_png(&dib)?)
    } else if let Some(data) = text {
        let units: Vec<_> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .take_while(|u| *u != 0)
            .collect();
        Content::TextUtf8(String::from_utf16_lossy(&units))
    } else {
        return Ok(None);
    };
    let len = match &content {
        Content::TextUtf8(s) => {
            ensure!(s.len() <= MAX_TEXT, "clipboard text exceeds 1 MiB");
            s.len()
        }
        Content::ImagePng(b) => {
            ensure!(b.len() <= MAX_WIRE, "clipboard PNG exceeds 8 MiB");
            png_dib(b)?;
            b.len()
        }
    };
    ensure!(
        len <= MAX_WIRE,
        "clipboard payload exceeds control channel limit"
    );
    Ok(Some(ClipboardSync {
        change_id: seq as u64,
        content: Some(content),
    }))
}

pub(super) fn write(
    hwnd: HWND,
    message: ClipboardSync,
    scope: &super::scoped_clipboard::Scope,
) -> Result<u32> {
    let mut formats = Vec::new();
    match message.content.context("empty clipboard update")? {
        Content::TextUtf8(value) => {
            ensure!(
                value.len() <= MAX_TEXT && !value.contains('\0'),
                "invalid or oversized clipboard text"
            );
            let bytes: Vec<_> = value
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            formats.push((TEXT, Block::copy(&bytes)?));
        }
        Content::ImagePng(data) => {
            ensure!(data.len() <= MAX_WIRE, "clipboard PNG exceeds 8 MiB");
            let format = png_format();
            ensure!(format != 0, "PNG clipboard format is unavailable");
            let dib = png_dib(&data)?;
            formats.push((format, Block::copy(&data)?));
            formats.push((DIBV5, Block::copy(&dib)?));
        }
    }
    let guard = Open::new(hwnd)?;
    scope.validate()?;
    ensure!(unsafe { EmptyClipboard() } != 0, "EmptyClipboard failed");
    for (format, block) in formats {
        block.publish(format)?;
    }
    drop(guard);
    Ok(unsafe { GetClipboardSequenceNumber() })
}

#[cfg(test)]
mod tests {
    #[test]
    fn png_dib_round_trip() {
        let pixels = [12, 42, 99, 127, 255, 0, 0, 255];
        let png = super::super::image::png(&pixels, 2, 1).unwrap();
        let dib = super::png_dib(&png).unwrap();
        assert_eq!(super::dib_png(&dib).unwrap(), png);
    }
}
