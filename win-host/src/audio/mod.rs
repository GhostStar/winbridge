use crate::{
    net::{Mux, AUDIO_OPUS, AUDIO_PCM_S16LE},
    proto::{control_message::Payload, HostError},
};
use anyhow::{anyhow, bail, ensure, Result};
use mousiki::{Application, Bitrate, Channels, Encoder};
use std::{
    collections::HashSet,
    ptr, slice,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::mpsc;
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

mod process;

pub(crate) const OPUS_SAMPLE_RATE: u32 = 48_000;
pub(crate) const OPUS_FRAME_SAMPLES_PER_CH: usize = 960; // 20ms at 48kHz
const OPUS_FRAME_INTERLEAVED_I16: usize = OPUS_FRAME_SAMPLES_PER_CH * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    PcmS16Le,
    Opus { bitrate_kbps: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapturePlan {
    /// Default render-endpoint loopback. Also used when the desktop is subscribed,
    /// or after process loopback fails to activate.
    System,
    /// Per-process scope is on with nothing unmuted, or every subscribed window is muted.
    Idle,
    /// One process-loopback client per pid, mixed into the single media audio stream.
    Processes(Vec<u32>),
    /// System loopback excluding a single muted process tree (WASAPI exclude mode).
    ExcludeProcess(u32),
}

/// `windows` is `(window_id, pid)` for subscribed roots only.
pub fn capture_plan(
    per_process: bool,
    windows: &[(u64, u32)],
    muted_window_ids: &HashSet<u64>,
) -> CapturePlan {
    let mut unmuted: Vec<(u64, u32)> = Vec::new();
    let mut muted: Vec<(u64, u32)> = Vec::new();
    for &(id, pid) in windows {
        if muted_window_ids.contains(&id) {
            muted.push((id, pid));
        } else {
            unmuted.push((id, pid));
        }
    }

    if !windows.is_empty() && unmuted.is_empty() {
        return CapturePlan::Idle;
    }

    let mut unmuted_pids: Vec<u32> = unmuted
        .iter()
        .filter_map(|(id, pid)| (*id != crate::win32::DESKTOP_WINDOW_ID && *pid != 0).then_some(*pid))
        .collect();
    unmuted_pids.sort_unstable();
    unmuted_pids.dedup();

    let unmuted_pid_set: HashSet<u32> = unmuted_pids.iter().copied().collect();
    let mut muted_only_pids: Vec<u32> = muted
        .iter()
        .filter_map(|(id, pid)| {
            (*id != crate::win32::DESKTOP_WINDOW_ID
                && *pid != 0
                && !unmuted_pid_set.contains(pid))
            .then_some(*pid)
        })
        .collect();
    muted_only_pids.sort_unstable();
    muted_only_pids.dedup();

    let desktop_unmuted = unmuted
        .iter()
        .any(|(id, pid)| *id == crate::win32::DESKTOP_WINDOW_ID || *pid == 0);
    let desktop_muted = muted
        .iter()
        .any(|(id, _)| *id == crate::win32::DESKTOP_WINDOW_ID);

    if !per_process {
        if muted.is_empty() {
            return CapturePlan::System;
        }
        if !desktop_muted && muted_only_pids.len() == 1 {
            return CapturePlan::ExcludeProcess(muted_only_pids[0]);
        }
        if !desktop_unmuted && !unmuted_pids.is_empty() && (desktop_muted || muted_only_pids.len() > 1) {
            return CapturePlan::Processes(unmuted_pids);
        }
        return CapturePlan::System;
    }

    if desktop_unmuted {
        if muted_only_pids.len() == 1 {
            return CapturePlan::ExcludeProcess(muted_only_pids[0]);
        }
        return CapturePlan::System;
    }

    if unmuted_pids.is_empty() {
        CapturePlan::Idle
    } else {
        CapturePlan::Processes(unmuted_pids)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleFormat {
    Float32,
    Int16,
    Int24,
    Int32,
}

pub(crate) struct ComGuard(bool);
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

pub(crate) struct MixFormatGuard(pub(crate) *mut WAVEFORMATEX);
impl Drop for MixFormatGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(Some(self.0.cast())) };
        }
    }
}

pub(crate) fn detect_format(ptr: *const WAVEFORMATEX) -> Result<(u16, usize, SampleFormat)> {
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

pub(crate) fn convert_to_stereo_i16(
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

pub(crate) fn stereo_i16_to_le_bytes(samples: &[i16]) -> Vec<u8> {
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

pub(crate) fn emit_interleaved_stereo(
    mux: &Mux,
    opus: &mut Option<OpusPipeline>,
    sample_rate: u16,
    stereo: &[i16],
) -> Result<()> {
    if stereo.is_empty() {
        return Ok(());
    }
    if let Some(enc) = opus.as_mut() {
        for packet in enc.encode_samples(stereo)? {
            mux.push_audio(AUDIO_OPUS, OPUS_SAMPLE_RATE as u16, 2, packet);
        }
    } else {
        mux.push_audio(
            AUDIO_PCM_S16LE,
            sample_rate,
            2,
            stereo_i16_to_le_bytes(stereo),
        );
    }
    Ok(())
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
    plan: CapturePlan,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioWorker {
    pub fn start(
        mux: Arc<Mux>,
        mode: AudioMode,
        plan: CapturePlan,
        replies: mpsc::Sender<Payload>,
    ) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = running.clone();
        let worker_mux = mux.clone();
        let worker_plan = plan.clone();
        let thread = std::thread::Builder::new()
            .name("audio-loopback".into())
            .spawn(move || {
                let _com = ComGuard::init();
                let mut plan = worker_plan;
                let mut reported_fallback = false;
                while worker_running.load(Ordering::Acquire) {
                    let step = match &plan {
                        CapturePlan::Idle => {
                            std::thread::sleep(Duration::from_millis(50));
                            Ok(process::RunOutcome::Stopped)
                        }
                        CapturePlan::System => {
                            run_once(&worker_mux, &worker_running, mode)
                                .map(|()| process::RunOutcome::Stopped)
                        }
                        CapturePlan::Processes(pids) => process::run_processes(
                            &worker_mux,
                            &worker_running,
                            mode,
                            pids,
                            false,
                            &mut |message| {
                                if !reported_fallback {
                                    reported_fallback = true;
                                    let _ = replies.try_send(Payload::HostError(HostError {
                                        window_id: 0,
                                        code: 0,
                                        message,
                                    }));
                                }
                            },
                        ),
                        CapturePlan::ExcludeProcess(pid) => process::run_processes(
                            &worker_mux,
                            &worker_running,
                            mode,
                            &[*pid],
                            true,
                            &mut |message| {
                                if !reported_fallback {
                                    reported_fallback = true;
                                    let _ = replies.try_send(Payload::HostError(HostError {
                                        window_id: 0,
                                        code: 0,
                                        message,
                                    }));
                                }
                            },
                        ),
                    };
                    match step {
                        Ok(process::RunOutcome::Stopped) => {
                            if matches!(
                                plan,
                                CapturePlan::System
                                    | CapturePlan::Processes(_)
                                    | CapturePlan::ExcludeProcess(_)
                            ) {
                                break;
                            }
                        }
                        Ok(process::RunOutcome::StreamEnded) => {
                            tracing::info!(
                                "process audio loopback stream ended; retrying after brief backoff"
                            );
                            std::thread::sleep(Duration::from_millis(250));
                        }
                        Err(e) => {
                            let invalidated = e.downcast_ref::<windows::core::Error>().is_some_and(
                                |we| we.code().0 as u32 == AUDCLNT_DEVICE_INVALIDATED,
                            );
                            if matches!(
                                plan,
                                CapturePlan::Processes(_) | CapturePlan::ExcludeProcess(_)
                            ) {
                                if !reported_fallback {
                                    reported_fallback = true;
                                    let _ = replies.try_send(Payload::HostError(HostError {
                                        window_id: 0,
                                        code: 0,
                                        message: format!(
                                            "进程回环不可用（需要 Windows 10 20348+ / Windows 11），已回退到系统环回：{e:#}"
                                        ),
                                    }));
                                }
                                tracing::warn!(error = %e, "process loopback failed; falling back to system loopback");
                                plan = CapturePlan::System;
                                continue;
                            }
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
            plan,
            thread: Some(thread),
        })
    }

    pub fn mode(&self) -> AudioMode {
        self.mode
    }

    pub fn plan(&self) -> &CapturePlan {
        &self.plan
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
    fn capture_plan_uses_system_for_desktop_and_handles_per_window_mute() {
        use crate::win32::DESKTOP_WINDOW_ID;
        let none = HashSet::new();
        assert_eq!(capture_plan(false, &[(10, 4)], &none), CapturePlan::System);
        assert_eq!(capture_plan(true, &[], &none), CapturePlan::Idle);
        assert_eq!(
            capture_plan(true, &[(DESKTOP_WINDOW_ID, 0), (10, 4)], &none),
            CapturePlan::System
        );
        assert_eq!(
            capture_plan(true, &[(10, 4), (11, 4), (12, 9)], &none),
            CapturePlan::Processes(vec![4, 9])
        );

        // Muting the only subscribed window pauses audio even in system scope.
        let mute_10: HashSet<u64> = [10].into_iter().collect();
        assert_eq!(capture_plan(false, &[(10, 4)], &mute_10), CapturePlan::Idle);

        // Muting one of two apps in system scope excludes just that PID.
        assert_eq!(
            capture_plan(false, &[(10, 4), (12, 9)], &mute_10),
            CapturePlan::ExcludeProcess(4)
        );

        // Muting one window when another window of the same PID is still unmuted keeps the PID audible.
        assert_eq!(
            capture_plan(true, &[(10, 4), (11, 4), (12, 9)], &mute_10),
            CapturePlan::Processes(vec![4, 9])
        );

        // Muting all windows of PID 4 in per-process scope drops PID 4.
        let mute_pid4: HashSet<u64> = [10, 11].into_iter().collect();
        assert_eq!(
            capture_plan(true, &[(10, 4), (11, 4), (12, 9)], &mute_pid4),
            CapturePlan::Processes(vec![9])
        );

        // Muting 2+ distinct PIDs in system scope narrows to unmuted subscribed PIDs.
        let mute_two: HashSet<u64> = [10, 12].into_iter().collect();
        assert_eq!(
            capture_plan(false, &[(10, 4), (12, 9), (13, 15)], &mute_two),
            CapturePlan::Processes(vec![15])
        );
    }

    #[test]
    fn mix_tracks_sums_active_streams_with_saturation_and_pads_silence() {
        let a = [1000i16, -20_000, 30_000, 0];
        let b: [i16; 0] = [];
        let c = [500i16, -20_000, 10_000, -500];
        let mixed = process::mix_tracks(&[&a, &b, &c], 2);
        assert_eq!(mixed, vec![1500, i16::MIN, i16::MAX, -500]);
        assert!(process::mix_tracks(&[&b, &[]], 2).iter().all(|s| *s == 0));
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
