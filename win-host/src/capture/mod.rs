mod d3d11;
mod wgc;

use crate::encode::{annexb::BitstreamCodec, mf_h264::VideoEncoder};
use anyhow::{ensure, Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::VecDeque,
    fs::File,
    path::Path,
    time::{Duration, Instant},
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

struct Reader {
    capture: wgc::Capture,
    staging: bool,
    width: u16,
    height: u16,
    texture: Option<ID3D11Texture2D>,
}

impl Reader {
    fn open(id: u64, cursor: bool, staging: bool, monitor_crop: bool) -> Result<Self> {
        Ok(Self {
            capture: wgc::Capture::new(id, cursor, monitor_crop)?,
            staging,
            width: 0,
            height: 0,
            texture: None,
        })
    }

    fn next(&mut self) -> Result<bool> {
        let Some((source, mut area)) = self.capture.next()? else {
            return Ok(false);
        };
        let mut w = u16::try_from(area.right - area.left)?;
        let mut h = u16::try_from(area.bottom - area.top)?;
        if !self.staging {
            // NV12 is 4:2:0. Omit the last row/column when a window has odd dimensions.
            w &= !1;
            h &= !1;
            area.right = area.left + u32::from(w);
            area.bottom = area.top + u32::from(h);
        }
        if w == 0 || h == 0 {
            self.capture.release()?;
            return Ok(false);
        }
        if self.width != w || self.height != h {
            self.texture = Some(self.capture.gpu.texture(w, h, self.staging)?);
            self.width = w;
            self.height = h;
        }
        self.capture
            .gpu
            .copy(&source, self.texture.as_ref().unwrap(), area);
        self.capture.release()?;
        Ok(true)
    }

    fn pixels(&self) -> Result<Vec<u8>> {
        self.capture.gpu.read(
            self.texture.as_ref().context("no capture available")?,
            self.width,
            self.height,
        )
    }
}

fn save_png(path: &Path, w: u16, h: u16, mut pixels: Vec<u8>) -> Result<()> {
    for p in pixels.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    let mut png = png::Encoder::new(
        std::io::BufWriter::new(File::create(path)?),
        w.into(),
        h.into(),
    );
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    png.write_header()?.write_image_data(&pixels)?;
    Ok(())
}

pub fn probe(
    id: u64,
    frames: u32,
    save: Option<&Path>,
    cursor: bool,
    monitor_crop: bool,
) -> Result<()> {
    ensure!(frames > 0, "--frames must be positive");
    let _apt = wgc::Apartment::new()?;
    let mut reader = Reader::open(id, cursor, true, monitor_crop)?;
    let start = Instant::now();
    let mut last = start;
    let mut count = 0;
    let mut read_time = Duration::ZERO;
    while count < frames {
        wgc::check_desktop(id)?;
        ensure!(
            !wgc::minimized(id),
            "CAPTURE_PAUSED_MINIMIZED: restore the selected window"
        );
        if reader.next()? {
            let read_start = Instant::now();
            let pixels = reader.pixels()?;
            read_time += read_start.elapsed();
            count += 1;
            last = Instant::now();
            if count == frames {
                let seconds = start.elapsed().as_secs_f64();
                println!("{count} frames, {}x{} physical px, {:.2} fps, {:.2} ms/frame, {:.2} ms/frame GPU readback",
                    reader.width,reader.height,count as f64/seconds,seconds*1000.0/count as f64,read_time.as_secs_f64()*1000.0/count as f64);
                if let Some(path) = save {
                    save_png(path, reader.width, reader.height, pixels)?;
                }
            }
        } else {
            ensure!(last.elapsed() < Duration::from_secs(12),
                "WGC produced no new frames for 12 seconds ({count}/{frames}); update the window contents and check the display connection");
        }
    }
    Ok(())
}

pub fn error_code(error: &anyhow::Error) -> i32 {
    let message = error.to_string();
    if message.starts_with("WINDOW_NOT_FOUND") {
        return 1;
    }
    let device_lost = error
        .downcast_ref::<windows::core::Error>()
        .is_some_and(|error| matches!(error.code().0 as u32, 0x887A0005..=0x887A0007 | 0x887A0026));
    if device_lost || message.starts_with("DISPLAY_DISCONNECTED") {
        return 4;
    }
    if message.starts_with("encoder:") {
        return 5;
    }
    2
}

#[allow(clippy::too_many_arguments)]
pub fn stream(
    id: u64,
    fps: u32,
    bitrate_kbps: u32,
    codec: i32,
    cursor: bool,
    monitor_crop: bool,
    running: &AtomicBool,
    keyframe: &AtomicBool,
    mut emit: impl FnMut(u16, u16, bool, Vec<u8>),
    mut report: impl FnMut(i32, String),
) -> Result<()> {
    let _apt = wgc::Apartment::new()?;
    ensure!(
        matches!(codec, 1 | 2 | 3 | 4),
        "encoder: unsupported codec {codec}"
    );
    let mf_codec = match codec {
        1 => Some(BitstreamCodec::H264),
        2 => Some(BitstreamCodec::Hevc),
        _ => None,
    };
    let is_desktop = id == crate::win32::DESKTOP_WINDOW_ID;
    let mut reader: Option<Reader> = None;
    let mut gdi_capturer: Option<crate::win32::desktop::SecureGdiCapturer> = None;
    let mut gdi_gpu: Option<(u16, u16, d3d11::Graphics, ID3D11Texture2D)> = None;
    let mut video_enc: Option<((u16, u16), VideoEncoder)> = None;
    let mut dirty = false;
    let mut paused = false;
    let interval = Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)));
    let jitter = (interval / 4).min(Duration::from_millis(2));
    let mut next_send = Instant::now();
    let mut measured = Duration::ZERO;
    let mut samples = 0;
    // Baseline H.264 has no picture reordering. Match async output to its capture
    // time so the diagnostic includes time spent waiting inside the encoder.
    let mut pending_capture = VecDeque::<Instant>::new();
    let mut deliver = |w, h, key, bytes, captured: Instant| {
        measured += captured.elapsed();
        samples += 1;
        emit(w, h, key, bytes);
        if samples == 300 {
            tracing::debug!(
                window_id = id,
                avg_frame_ms = measured.as_secs_f64() * 1000.0 / 300.0,
                "capture to encoded output (excludes waiting for the next WGC frame)"
            );
            measured = Duration::ZERO;
            samples = 0;
        }
    };
    while running.load(Ordering::Acquire) {
        wgc::check_desktop(id)?;

        // When streaming the full primary desktop and Windows switches into a Secure Desktop
        // (UAC consent prompt / Lock Screen), WGC pauses. Run GDI BitBlt on a dedicated clean
        // thread bound to Winlogon while keeping D3D11 + Media Foundation H.264 on Default.
        if is_desktop && crate::win32::desktop::is_secure_desktop() {
            if gdi_capturer.is_none() {
                reader = None;
                video_enc = None;
                pending_capture.clear();
                gdi_gpu = None;
                dirty = false;
                gdi_capturer = Some(crate::win32::desktop::SecureGdiCapturer::start()?);
            }
            if let Some((raw_w, raw_h, raw_pixels)) = gdi_capturer
                .as_ref()
                .and_then(|cap| cap.recv_timeout(Duration::from_millis(60)))
            {
                let frame_start = Instant::now();
                let w = if mf_codec.is_some() { raw_w & !1 } else { raw_w };
                let h = if mf_codec.is_some() { raw_h & !1 } else { raw_h };
                if w >= 2 && h >= 2 {
                    let pixels = if w == raw_w && h == raw_h {
                        raw_pixels
                    } else {
                        let mut cropped = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
                        for y in 0..usize::from(h) {
                            let row_start = y * usize::from(raw_w) * 4;
                            cropped.extend_from_slice(
                                &raw_pixels[row_start..row_start + usize::from(w) * 4],
                            );
                        }
                        cropped
                    };
                    let force = keyframe.swap(false, Ordering::AcqRel);
                    if let Some(bs_codec) = mf_codec {
                        let need_gpu = gdi_gpu
                            .as_ref()
                            .is_none_or(|(gw, gh, _, _)| *gw != w || *gh != h);
                        if need_gpu {
                            video_enc = None;
                            pending_capture.clear();
                            let gpu = d3d11::Graphics::new()?;
                            let tex = gpu.texture(w, h, false)?;
                            gdi_gpu = Some((w, h, gpu, tex));
                        }
                        let (_, _, gpu, tex) = gdi_gpu.as_ref().unwrap();
                        unsafe {
                            gpu.ctx.UpdateSubresource(
                                tex,
                                0,
                                None,
                                pixels.as_ptr().cast(),
                                u32::from(w) * 4,
                                0,
                            );
                        }
                        let resized = video_enc
                            .as_ref()
                            .is_none_or(|(dims, _)| *dims != (w, h));
                        if resized {
                            drop(video_enc.take());
                            pending_capture.clear();
                            let enc = VideoEncoder::new(
                                bs_codec,
                                &gpu.device,
                                &gpu.ctx,
                                w,
                                h,
                                fps.min(30),
                                bitrate_kbps,
                            )
                            .context("encoder: cannot initialize video encoder for secure desktop")?;
                            video_enc = Some(((w, h), enc));
                        }
                        let (_, enc) = video_enc.as_mut().unwrap();
                        ensure!(
                            pending_capture.len() < 64,
                            "encoder: video MFT has stopped returning frames"
                        );
                        pending_capture.push_back(frame_start);
                        for (key, data) in enc
                            .encode(tex, force || resized)
                            .context("encoder: secure desktop encode failed")?
                        {
                            let captured = pending_capture
                                .pop_front()
                                .unwrap_or(frame_start);
                            deliver(w, h, key, data, captured);
                        }
                    } else {
                        let payload = if codec == 3 {
                            let mut jpeg = Vec::new();
                            jpeg_encoder::Encoder::new(&mut jpeg, 85)
                                .encode(&pixels, w, h, jpeg_encoder::ColorType::Bgra)
                                .context("encoder: JPEG compression failed")?;
                            jpeg
                        } else {
                            pixels
                        };
                        deliver(w, h, true, payload, frame_start);
                    }
                }
            }
            continue;
        } else if gdi_capturer.is_some() {
            gdi_capturer = None;
            video_enc = None;
            pending_capture.clear();
            gdi_gpu = None;
            dirty = false;
            keyframe.store(true, Ordering::Release);
        }

        if wgc::minimized(id) {
            if !paused {
                report(
                    3,
                    "CAPTURE_PAUSED_MINIMIZED: restore the window to resume capture".into(),
                );
                video_enc = None;
                pending_capture.clear();
                reader = None;
                dirty = false;
                paused = true;
            }
            std::thread::sleep(Duration::from_millis(80));
            continue;
        }
        paused = false;
        if reader.as_ref().is_some_and(|r| r.capture.monitor_changed()) {
            video_enc = None;
            pending_capture.clear();
            reader = None;
            dirty = false;
        }
        if reader.is_none() {
            reader = Some(Reader::open(id, cursor, mf_codec.is_none(), monitor_crop)?);
            keyframe.store(true, Ordering::Release);
        }
        let src = reader.as_mut().unwrap();
        let fresh = src.next()?;
        dirty |= fresh;
        let now = Instant::now();
        if !fresh {
            if let Some(((w, h), encoder)) = video_enc.as_mut() {
                for (key, data) in encoder
                    .drain_ready()
                    .context("encoder: draining asynchronous video output")?
                {
                    let captured = pending_capture
                        .pop_front()
                        .context("unexpected video encoder output")?;
                    deliver(*w, *h, key, data, captured);
                }
            }
        }
        let request = keyframe.load(Ordering::Acquire);
        if src.texture.is_none() || (!request && (!dirty || now + jitter < next_send)) {
            continue;
        }
        let start = if fresh { src.capture.frame_start } else { now };
        let force = keyframe.swap(false, Ordering::AcqRel);
        dirty = false;
        let w = src.width;
        let h = src.height;
        ensure!(
            codec != 4 || u64::from(w) * u64::from(h) * 4 <= 64 * 1024 * 1024,
            "encoder: RAW_BGRA frame exceeds 64 MiB"
        );
        if let Some(bs_codec) = mf_codec {
            let resized = video_enc
                .as_ref()
                .is_none_or(|(dimensions, _)| *dimensions != (w, h));
            if resized {
                drop(video_enc.take());
                pending_capture.clear();
                let gpu = &src.capture.gpu;
                let encoder = VideoEncoder::new(bs_codec, &gpu.device, &gpu.ctx, w, h, fps, bitrate_kbps)
                    .context("encoder: cannot initialize video encoder")?;
                video_enc = Some(((w, h), encoder));
            }
            let (_, encoder) = video_enc.as_mut().unwrap();
            ensure!(
                pending_capture.len() < 64,
                "encoder: video MFT has stopped returning frames"
            );
            pending_capture.push_back(start);
            let packets = encoder
                .encode(src.texture.as_ref().unwrap(), force || resized)
                .context("encoder: video encoding failed")?;
            for (key, data) in packets {
                let captured = pending_capture
                    .pop_front()
                    .context("unexpected video encoder output")?;
                deliver(w, h, key, data, captured);
            }
        } else {
            let pixels = src.pixels()?;
            let payload = if codec == 3 {
                let mut jpeg = Vec::new();
                jpeg_encoder::Encoder::new(&mut jpeg, 85)
                    .encode(&pixels, w, h, jpeg_encoder::ColorType::Bgra)
                    .context("encoder: JPEG compression failed")?;
                jpeg
            } else {
                pixels
            };
            deliver(w, h, true, payload, start);
        }
        // Advance on the original clock even after a slow frame. An early keyframe
        // is an out-of-band recovery point and should not delay the next regular frame.
        if now + jitter >= next_send {
            let late_ns = now.saturating_duration_since(next_send).as_nanos();
            let phase = Duration::from_nanos((late_ns % interval.as_nanos()) as u64);
            next_send = now.max(next_send) + interval - phase;
        }
    }
    Ok(())
}

pub fn supports_h264() -> bool {
    let probe = || -> Result<()> {
        let _apt = wgc::Apartment::new()?;
        let gpu = d3d11::Graphics::new()?;
        let _encoder = VideoEncoder::new(BitstreamCodec::H264, &gpu.device, &gpu.ctx, 640, 480, 30, 4000)?;
        Ok(())
    };
    if let Err(error) = probe() {
        tracing::warn!(reason=%format!("{error:#}"), "H.264 initialization probe failed");
        return false;
    }
    true
}

pub fn supports_hevc() -> bool {
    let probe = || -> Result<()> {
        let _apt = wgc::Apartment::new()?;
        let gpu = d3d11::Graphics::new()?;
        let _encoder = VideoEncoder::new(BitstreamCodec::Hevc, &gpu.device, &gpu.ctx, 640, 480, 30, 4000)?;
        Ok(())
    };
    if let Err(error) = probe() {
        tracing::info!(reason=%format!("{error:#}"), "HEVC hardware encoder probe unavailable");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::video_processor::VideoProcessor;

    #[test]
    #[ignore = "requires an active D3D11 video processor"]
    fn gpu_nv12_preserves_color_and_removes_row_padding() -> Result<()> {
        let _apt = wgc::Apartment::new()?;
        let gpu = d3d11::Graphics::new()?;
        let w = 130u16;
        let h = 72u16;
        let source = gpu.texture(w, h, false)?;
        let bgra = [0u8, 0, 255, 255].repeat(w as usize * h as usize);
        unsafe {
            gpu.ctx
                .UpdateSubresource(&source, 0, None, bgra.as_ptr().cast(), u32::from(w) * 4, 0)
        };
        let mut converter = VideoProcessor::new(&gpu.device, &gpu.ctx, w, h, 30)?;
        let frame = converter.convert(&source)?;
        let nv12 = converter.read(&frame)?;
        let luma = w as usize * h as usize;
        assert_eq!(nv12.len(), luma * 3 / 2);
        // BT.709 studio range maps pure red to approximately (63, 102, 240).
        assert!(
            nv12[..luma].iter().all(|&v| v.abs_diff(63) <= 3),
            "unexpected luma {}",
            nv12[0]
        );
        assert!(
            nv12[luma..]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|uv| uv[0].abs_diff(102) <= 3 && uv[1].abs_diff(240) <= 3),
            "unexpected chroma {:?}",
            &nv12[luma..luma + 2]
        );
        Ok(())
    }
}
