use crate::net::Mux;
use anyhow::{bail, ensure, Result};
use std::{
    ptr, slice,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use windows::{
    core::GUID,
    Win32::{
        Media::Audio::{
            eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
            MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
            WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
            COINIT_MULTITHREADED,
        },
    },
};

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
    GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const BUFFER_FLAG_SILENT: u32 = 0x0000_0002;
const AUDCLNT_DEVICE_INVALIDATED: u32 = 0x8889_0004;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleFormat {
    Float32,
    Int16,
    Int24,
    Int32,
}

struct ComGuard(bool);
impl ComGuard {
    fn init() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self(hr.is_ok())
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

struct MixFormatGuard(*mut WAVEFORMATEX);
impl Drop for MixFormatGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(Some(self.0.cast())) };
        }
    }
}

fn detect_format(ptr: *const WAVEFORMATEX) -> Result<(u16, usize, SampleFormat)> {
    ensure!(!ptr.is_null(), "WASAPI returned null mix format");
    let mix = unsafe { ptr::read_unaligned(ptr) };
    let sample_rate = u16::try_from(mix.nSamplesPerSec)
        .map_err(|_| anyhow::anyhow!("unsupported sample rate {} Hz", mix.nSamplesPerSec))?;
    let channels = usize::from(mix.nChannels);
    ensure!(channels > 0, "WASAPI mix format has 0 channels");

    let format = match mix.wFormatTag {
        WAVE_FORMAT_IEEE_FLOAT if mix.wBitsPerSample == 32 => SampleFormat::Float32,
        WAVE_FORMAT_PCM => match mix.wBitsPerSample {
            16 => SampleFormat::Int16,
            24 => SampleFormat::Int24,
            32 => SampleFormat::Int32,
            other => bail!("unsupported WASAPI PCM bit depth {other}"),
        },
        WAVE_FORMAT_EXTENSIBLE => {
            let ext = ptr as *const WAVEFORMATEXTENSIBLE;
            let sub = unsafe { ptr::read_unaligned(ptr::addr_of!((*ext).SubFormat)) };
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && mix.wBitsPerSample == 32 {
                SampleFormat::Float32
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
                match mix.wBitsPerSample {
                    16 => SampleFormat::Int16,
                    24 => SampleFormat::Int24,
                    32 => SampleFormat::Int32,
                    other => bail!("unsupported extensible PCM bit depth {other}"),
                }
            } else {
                bail!("unsupported extensible audio subtype {sub:?}");
            }
        }
        tag => bail!(
            "unsupported WASAPI format tag 0x{tag:04x} ({} bits)",
            mix.wBitsPerSample
        ),
    };
    Ok((sample_rate, channels, format))
}

fn float_to_i16(v: f32) -> i16 {
    let clamped = if v.is_finite() {
        v.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    (clamped * 32767.0).round() as i16
}

fn convert_to_stereo_s16le(
    raw: &[u8],
    frames: usize,
    channels: usize,
    fmt: SampleFormat,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames * 4);
    let sample_bytes = match fmt {
        SampleFormat::Int16 => 2,
        SampleFormat::Int24 => 3,
        SampleFormat::Float32 | SampleFormat::Int32 => 4,
    };
    let frame_stride = channels * sample_bytes;
    let available_frames = (raw.len() / frame_stride).min(frames);

    let read_sample = |frame_slice: &[u8], ch: usize| -> i16 {
        let off = ch * sample_bytes;
        match fmt {
            SampleFormat::Float32 => {
                let v = f32::from_le_bytes([
                    frame_slice[off],
                    frame_slice[off + 1],
                    frame_slice[off + 2],
                    frame_slice[off + 3],
                ]);
                float_to_i16(v)
            }
            SampleFormat::Int16 => i16::from_le_bytes([frame_slice[off], frame_slice[off + 1]]),
            SampleFormat::Int24 => {
                // Top 16 bits of signed 24-bit little-endian sample.
                i16::from_le_bytes([frame_slice[off + 1], frame_slice[off + 2]])
            }
            SampleFormat::Int32 => {
                // Top 16 bits of signed 32-bit little-endian sample.
                i16::from_le_bytes([frame_slice[off + 2], frame_slice[off + 3]])
            }
        }
    };

    for i in 0..available_frames {
        let frame_slice = &raw[i * frame_stride..(i + 1) * frame_stride];
        let left = read_sample(frame_slice, 0);
        let right = if channels >= 2 {
            read_sample(frame_slice, 1)
        } else {
            left
        };
        out.extend_from_slice(&left.to_le_bytes());
        out.extend_from_slice(&right.to_le_bytes());
    }
    out
}

fn run_once(mux: &Mux, running: &AtomicBool) -> Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix_guard = MixFormatGuard(client.GetMixFormat()?);
        let (sample_rate, channels, fmt) = detect_format(mix_guard.0)?;

        // 20ms shared loopback buffer (units are 100ns).
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            200_000,
            0,
            mix_guard.0,
            None,
        )?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        tracing::info!(
            sample_rate,
            channels,
            ?fmt,
            "WASAPI system audio loopback started"
        );

        let mut silent_streak = 0u32;
        let loop_result = (|| -> Result<()> {
            while running.load(Ordering::Acquire) {
                let mut packet_frames = capture.GetNextPacketSize()?;
                if packet_frames == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                while packet_frames > 0 && running.load(Ordering::Acquire) {
                    let mut data_ptr = ptr::null_mut();
                    let mut num_frames = 0u32;
                    let mut flags = 0u32;
                    capture.GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)?;
                    let silent = (flags & BUFFER_FLAG_SILENT) != 0 || data_ptr.is_null();
                    let frames = num_frames as usize;
                    let payload = if frames == 0 {
                        None
                    } else if silent {
                        silent_streak = silent_streak.saturating_add(1);
                        if silent_streak <= 2 {
                            Some(vec![0u8; frames * 4])
                        } else {
                            None
                        }
                    } else {
                        silent_streak = 0;
                        let sample_bytes = match fmt {
                            SampleFormat::Int16 => 2,
                            SampleFormat::Int24 => 3,
                            SampleFormat::Float32 | SampleFormat::Int32 => 4,
                        };
                        let raw = slice::from_raw_parts(data_ptr, frames * channels * sample_bytes);
                        Some(convert_to_stereo_s16le(raw, frames, channels, fmt))
                    };
                    capture.ReleaseBuffer(num_frames)?;
                    if let Some(bytes) = payload {
                        if !bytes.is_empty() {
                            mux.push_audio(sample_rate, 2, bytes);
                        }
                    }
                    packet_frames = capture.GetNextPacketSize()?;
                }
            }
            Ok(())
        })();

        let _ = client.Stop();
        loop_result
    }
}

pub struct AudioWorker {
    running: Arc<AtomicBool>,
    mux: Arc<Mux>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioWorker {
    pub fn start(mux: Arc<Mux>) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = running.clone();
        let worker_mux = mux.clone();
        let thread = std::thread::Builder::new()
            .name("audio-loopback".into())
            .spawn(move || {
                let _com = ComGuard::init();
                while worker_running.load(Ordering::Acquire) {
                    match run_once(&worker_mux, &worker_running) {
                        Ok(()) => break,
                        Err(e) => {
                            let invalidated = e
                                .downcast_ref::<windows::core::Error>()
                                .is_some_and(|we| we.code().0 as u32 == AUDCLNT_DEVICE_INVALIDATED);
                            if invalidated {
                                tracing::info!(
                                    "Windows audio output device changed; reopening loopback"
                                );
                                std::thread::sleep(Duration::from_millis(200));
                            } else {
                                tracing::warn!(error = %e, "WASAPI audio loopback error; retrying");
                                std::thread::sleep(Duration::from_millis(800));
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            running,
            mux,
            thread: Some(thread),
        })
    }
}

impl Drop for AudioWorker {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            if tokio::runtime::Handle::try_current()
                .is_ok_and(|rt| rt.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
            {
                let _ = tokio::task::block_in_place(|| handle.join());
            } else {
                let _ = handle.join();
            }
        }
        self.mux.clear_audio();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_float32_mono_stereo_and_multichannel_to_stereo_s16le() {
        // Stereo Float32: (0.5, -1.0) and (2.0 clamped, -2.0 clamped)
        let mut raw = Vec::new();
        for f in [0.5f32, -1.0, 2.0, -2.0] {
            raw.extend_from_slice(&f.to_le_bytes());
        }
        let pcm = convert_to_stereo_s16le(&raw, 2, 2, SampleFormat::Float32);
        assert_eq!(pcm.len(), 8);
        let samples: Vec<i16> = pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples, vec![16384, -32767, 32767, -32767]);

        // Mono Float32 duplicates to both channels
        let mono: Vec<u8> = (-0.5f32).to_le_bytes().to_vec();
        let pcm_mono = convert_to_stereo_s16le(&mono, 1, 1, SampleFormat::Float32);
        let s_mono: Vec<i16> = pcm_mono
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(s_mono, vec![-16384, -16384]);
    }
}
