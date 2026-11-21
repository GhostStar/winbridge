use crate::net::{Mux, AUDIO_OPUS, AUDIO_PCM_S16LE};
use anyhow::{anyhow, bail, ensure, Result};
use mousiki::{Application, Bitrate, Channels, Encoder};
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

pub(crate) mod mic;

const OPUS_SAMPLE_RATE: u32 = 48_000;
const OPUS_FRAME_SAMPLES_PER_CH: usize = 960; // 20ms at 48kHz
const OPUS_FRAME_INTERLEAVED_I16: usize = OPUS_FRAME_SAMPLES_PER_CH * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    PcmS16Le,
    Opus { bitrate_kbps: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleFormat {
    Float32,
    Int16,
    Int24,
    Int32,
}

pub(crate) struct ComGuard(bool);
impl ComGuard {
    pub(crate) fn init() -> Self {
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
    let sample_rate_raw = mix.nSamplesPerSec;
    let bits = mix.wBitsPerSample;
    let channels_raw = mix.nChannels;
    let sample_rate = u16::try_from(sample_rate_raw)
        .map_err(|_| anyhow!("unsupported sample rate {} Hz", sample_rate_raw))?;
    let channels = usize::from(channels_raw);
    ensure!(channels > 0, "WASAPI mix format has 0 channels");

    let format = match mix.wFormatTag {
        WAVE_FORMAT_IEEE_FLOAT if bits == 32 => SampleFormat::Float32,
        WAVE_FORMAT_PCM => match bits {
            16 => SampleFormat::Int16,
            24 => SampleFormat::Int24,
            32 => SampleFormat::Int32,
            other => bail!("unsupported WASAPI PCM bit depth {other}"),
        },
        WAVE_FORMAT_EXTENSIBLE => {
            let ext = ptr as *const WAVEFORMATEXTENSIBLE;
            let sub = unsafe { ptr::read_unaligned(ptr::addr_of!((*ext).SubFormat)) };
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && bits == 32 {
                SampleFormat::Float32
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
                match bits {
                    16 => SampleFormat::Int16,
                    24 => SampleFormat::Int24,
                    32 => SampleFormat::Int32,
                    other => bail!("unsupported extensible PCM bit depth {other}"),
                }
            } else {
                bail!("unsupported extensible audio subtype {sub:?}");
            }
        }
        tag => bail!("unsupported WASAPI format tag 0x{tag:04x} ({} bits)", bits),
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

fn convert_to_stereo_i16(
    raw: &[u8],
    frames: usize,
    channels: usize,
    fmt: SampleFormat,
) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * 2);
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
            SampleFormat::Int24 => i16::from_le_bytes([frame_slice[off + 1], frame_slice[off + 2]]),
            SampleFormat::Int32 => i16::from_le_bytes([frame_slice[off + 2], frame_slice[off + 3]]),
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
        out.push(left);
        out.push(right);
    }
    out
}

fn stereo_i16_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Lightweight streaming linear stereo resampler targeting 48 kHz for RFC 6716 Opus framing.
/// Passes 48 kHz input through without copying overhead.
pub(crate) struct StereoResampler {
    src_rate: u32,
    dst_rate: u32,
    pos: f64,
    prev: [i16; 2],
    has_prev: bool,
}

impl StereoResampler {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            src_rate: src_rate.max(1),
            dst_rate: dst_rate.max(1),
            pos: 0.0,
            prev: [0, 0],
            has_prev: false,
        }
    }

    pub fn push(&mut self, interleaved_stereo: &[i16], out: &mut Vec<i16>) {
        let in_frames = interleaved_stereo.len() / 2;
        if in_frames == 0 {
            return;
        }
        if self.src_rate == self.dst_rate {
            out.extend_from_slice(&interleaved_stereo[..in_frames * 2]);
            return;
        }
        let step = f64::from(self.src_rate) / f64::from(self.dst_rate);
        let sample_at = |idx: isize| -> [f64; 2] {
            if idx < 0 {
                if self.has_prev {
                    [f64::from(self.prev[0]), f64::from(self.prev[1])]
                } else {
                    [
                        f64::from(interleaved_stereo[0]),
                        f64::from(interleaved_stereo[1]),
                    ]
                }
            } else {
                let i = (idx as usize).min(in_frames - 1);
                [
                    f64::from(interleaved_stereo[i * 2]),
                    f64::from(interleaved_stereo[i * 2 + 1]),
                ]
            }
        };

        while self.pos < in_frames as f64 {
            let i0 = self.pos.floor() as isize;
            let frac = self.pos - (i0 as f64);
            let s0 = sample_at(i0);
            let s1 = sample_at(i0 + 1);
            let l = (s0[0] + (s1[0] - s0[0]) * frac).round().clamp(-32768.0, 32767.0) as i16;
            let r = (s0[1] + (s1[1] - s0[1]) * frac).round().clamp(-32768.0, 32767.0) as i16;
            out.push(l);
            out.push(r);
            self.pos += step;
        }
        self.pos -= in_frames as f64;
        self.prev = [
            interleaved_stereo[(in_frames - 1) * 2],
            interleaved_stereo[(in_frames - 1) * 2 + 1],
        ];
        self.has_prev = true;
    }
}

pub(crate) struct OpusPipeline {
    resampler: StereoResampler,
    encoder: Encoder,
    pending: Vec<i16>,
    packet_buf: Vec<u8>,
}

impl OpusPipeline {
    pub fn new(src_rate: u32, bitrate_kbps: u32) -> Result<Self> {
        let mut encoder = Encoder::new(OPUS_SAMPLE_RATE, Channels::Stereo, Application::Audio)
            .map_err(|e| anyhow!("init pure-Rust Opus encoder: {e:?}"))?;
        let bps = bitrate_kbps.clamp(24, 510).saturating_mul(1000) as i32;
        encoder
            .set_bitrate(Bitrate::Bits(bps))
            .map_err(|e| anyhow!("set Opus bitrate {bps} bps: {e:?}"))?;
        Ok(Self {
            resampler: StereoResampler::new(src_rate, OPUS_SAMPLE_RATE),
            encoder,
            pending: Vec::with_capacity(OPUS_FRAME_INTERLEAVED_I16 * 4),
            packet_buf: vec![0u8; 4000],
        })
    }

    pub fn encode_samples(&mut self, stereo_i16: &[i16]) -> Result<Vec<Vec<u8>>> {
        self.resampler.push(stereo_i16, &mut self.pending);
        let mut packets = Vec::new();
        while self.pending.len() >= OPUS_FRAME_INTERLEAVED_I16 {
            let len = self
                .encoder
                .encode(
                    &self.pending[..OPUS_FRAME_INTERLEAVED_I16],
                    &mut self.packet_buf,
                )
                .map_err(|e| anyhow!("Opus encode failed: {e:?}"))?;
            self.pending.drain(..OPUS_FRAME_INTERLEAVED_I16);
            if len > 0 {
                packets.push(self.packet_buf[..len].to_vec());
            }
        }
        Ok(packets)
    }
}

fn run_once(mux: &Mux, running: &AtomicBool, mode: AudioMode) -> Result<()> {
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
        let mut opus = match mode {
            AudioMode::PcmS16Le => None,
            AudioMode::Opus { bitrate_kbps } => {
                Some(OpusPipeline::new(u32::from(sample_rate), bitrate_kbps)?)
            }
        };
        client.Start()?;
        tracing::info!(
            sample_rate,
            channels,
            ?fmt,
            ?mode,
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
                    let samples = if frames == 0 {
                        None
                    } else if silent {
                        silent_streak = silent_streak.saturating_add(1);
                        if silent_streak <= 2 {
                            Some(vec![0i16; frames * 2])
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
                        Some(convert_to_stereo_i16(raw, frames, channels, fmt))
                    };
                    capture.ReleaseBuffer(num_frames)?;
                    if let Some(stereo) = samples {
                        if !stereo.is_empty() {
                            if let Some(enc) = opus.as_mut() {
                                for packet in enc.encode_samples(&stereo)? {
                                    mux.push_audio(AUDIO_OPUS, OPUS_SAMPLE_RATE as u16, 2, packet);
                                }
                            } else {
                                mux.push_audio(
                                    AUDIO_PCM_S16LE,
                                    sample_rate,
                                    2,
                                    stereo_i16_to_le_bytes(&stereo),
                                );
                            }
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
    mode: AudioMode,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioWorker {
    pub fn start(mux: Arc<Mux>, mode: AudioMode) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = running.clone();
        let worker_mux = mux.clone();
        let thread = std::thread::Builder::new()
            .name("audio-loopback".into())
            .spawn(move || {
                let _com = ComGuard::init();
                while worker_running.load(Ordering::Acquire) {
                    match run_once(&worker_mux, &worker_running, mode) {
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
            mode,
            thread: Some(thread),
        })
    }

    pub fn mode(&self) -> AudioMode {
        self.mode
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
        let mut raw = Vec::new();
        for f in [0.5f32, -1.0, 2.0, -2.0] {
            raw.extend_from_slice(&f.to_le_bytes());
        }
        let samples = convert_to_stereo_i16(&raw, 2, 2, SampleFormat::Float32);
        assert_eq!(samples, vec![16384, -32767, 32767, -32767]);
        assert_eq!(stereo_i16_to_le_bytes(&samples).len(), 8);

        let mono: Vec<u8> = (-0.5f32).to_le_bytes().to_vec();
        let s_mono = convert_to_stereo_i16(&mono, 1, 1, SampleFormat::Float32);
        assert_eq!(s_mono, vec![-16384, -16384]);
    }

    #[test]
    fn resamples_44100_to_48000_and_encodes_valid_opus_packets() {
        // Generate 40ms of 440Hz stereo sine wave at 44.1 kHz
        let src_frames = 1764usize; // 40ms @ 44.1kHz
        let mut pcm = Vec::with_capacity(src_frames * 2);
        for i in 0..src_frames {
            let phase = 2.0 * std::f32::consts::PI * 440.0 * (i as f32) / 44_100.0;
            let sample = (phase.sin() * 12_000.0) as i16;
            pcm.push(sample);
            pcm.push(sample);
        }
        let mut pipeline = OpusPipeline::new(44_100, 128).unwrap();
        let packets = pipeline.encode_samples(&pcm).unwrap();
        assert_eq!(packets.len(), 2, "40ms of audio should emit two 20ms Opus packets");
        for pkt in &packets {
            // Raw 20ms 48kHz stereo PCM is 3840 bytes; 128kbps Opus should be ~200..600 bytes.
            assert!(
                !pkt.is_empty() && pkt.len() < 1200,
                "unexpected Opus packet size {}",
                pkt.len()
            );
        }
    }
}
