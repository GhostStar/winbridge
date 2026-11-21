//! Per-process WASAPI loopback (Windows 10 20348+ / Windows 11).
//!
//! `windows` 0.58 does not expose `IActivateAudioInterfaceCompletionHandler`, so
//! activation goes through `ActivateAudioInterfaceAsync` in mmdevapi.dll and a
//! tiny COM completion object. Captured streams are resampled to 48 kHz stereo,
//! mixed, and pushed as the single existing audio media stream (window id 0).

use super::{
    convert_to_stereo_i16, emit_interleaved_stereo, AudioMode, OpusPipeline, SampleFormat,
    OPUS_FRAME_SAMPLES_PER_CH, OPUS_SAMPLE_RATE, WAVE_FORMAT_PCM,
};
use crate::net::Mux;
use anyhow::{bail, Result};
use std::{
    ffi::c_void,
    mem, ptr,
    sync::{
        atomic::{AtomicI32, AtomicPtr, Ordering},
        OnceLock,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, GUID},
    Win32::Media::Audio::{
        IAudioCaptureClient, IAudioClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
        WAVEFORMATEX,
    },
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::{
        LibraryLoader::{GetProcAddress, LoadLibraryW},
        Threading::{CreateEventW, SetEvent, WaitForSingleObject},
    },
};

const E_POINTER: i32 = 0x8000_4003_u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;
const E_FAIL: i32 = 0x8000_4005_u32 as i32;
const BUFFER_FLAG_SILENT: u32 = 0x0000_0002;
const VT_BLOB: u16 = 65;
const ACTIVATION_PROCESS_LOOPBACK: u32 = 1;
const LOOPBACK_INCLUDE_TREE: u32 = 0;
const LOOPBACK_EXCLUDE_TREE: u32 = 1;

const IID_UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_C000_000000000046);
const IID_HANDLER: GUID = GUID::from_u128(0x41D949AB_9862_444A_80F6_C261334DA5EB);
const IID_AGILE_OBJECT: GUID = GUID::from_u128(0x94EA2B94_E9CC_49E0_C0FF_EE64CA8F5B90);
const IID_AUDIO_CLIENT: GUID = GUID::from_u128(0x1CB9AD4C_DBFA_4c32_B178_C2F568A703B2);

// Keep at most 100ms (5 x 20ms stereo frames) buffered per process to prevent clock-drift lag.
const MAX_PENDING_SAMPLES: usize = OPUS_FRAME_SAMPLES_PER_CH * 2 * 5;

#[repr(C)]
struct ActivationParams {
    activation_type: u32,
    target_process_id: u32,
    process_loopback_mode: u32,
}

#[repr(C)]
struct BlobPropVariant {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    cb_size: u32,
    _pad: u32,
    data: *mut u8,
}

type ActivateAudioInterfaceAsyncFn = unsafe extern "system" fn(
    path: *const u16,
    riid: *const GUID,
    params: *const BlobPropVariant,
    handler: *mut c_void,
    operation: *mut *mut c_void,
) -> i32;

#[repr(C)]
struct HandlerVtbl {
    query_interface:
        unsafe extern "system" fn(*mut Handler, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut Handler) -> u32,
    release: unsafe extern "system" fn(*mut Handler) -> u32,
    activate_completed: unsafe extern "system" fn(*mut Handler, *mut c_void) -> i32,
}

#[repr(C)]
struct Handler {
    vtbl: *const HandlerVtbl,
    refs: AtomicI32,
    event: HANDLE,
    hr: AtomicI32,
    client: AtomicPtr<c_void>,
}

static HANDLER_VTBL: HandlerVtbl = HandlerVtbl {
    query_interface: handler_qi,
    add_ref: handler_add,
    release: handler_release,
    activate_completed: handler_completed,
};

unsafe extern "system" fn handler_qi(
    this: *mut Handler,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> i32 {
    if this.is_null() || iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    let id = &*iid;
    if *id != IID_UNKNOWN && *id != IID_HANDLER && *id != IID_AGILE_OBJECT {
        *out = ptr::null_mut();
        return E_NOINTERFACE;
    }
    *out = this.cast();
    handler_add(this);
    0
}

unsafe extern "system" fn handler_add(this: *mut Handler) -> u32 {
    if this.is_null() {
        return 0;
    }
    (*this).refs.fetch_add(1, Ordering::Relaxed) as u32 + 1
}

unsafe extern "system" fn handler_release(this: *mut Handler) -> u32 {
    if this.is_null() {
        return 0;
    }
    let left = (*this).refs.fetch_sub(1, Ordering::Release) - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        let handler = Box::from_raw(this);
        if !handler.event.is_null() {
            CloseHandle(handler.event);
        }
        let leftover = handler.client.load(Ordering::Acquire);
        if !leftover.is_null() {
            release_unknown(leftover);
        }
        0
    } else {
        left as u32
    }
}

unsafe extern "system" fn handler_completed(this: *mut Handler, op: *mut c_void) -> i32 {
    if this.is_null() {
        return E_POINTER;
    }
    if !op.is_null() {
        let vtbl = *(op as *const *const usize);
        let get_result: unsafe extern "system" fn(*mut c_void, *mut i32, *mut *mut c_void) -> i32 =
            mem::transmute(*vtbl.add(3));
        let mut hr = E_FAIL;
        let mut punk = ptr::null_mut();
        let call_hr = get_result(op, &mut hr, &mut punk);
        if call_hr < 0 {
            (*this).hr.store(call_hr, Ordering::Release);
        } else {
            (*this).hr.store(hr, Ordering::Release);
            (*this).client.store(punk, Ordering::Release);
        }
    } else {
        (*this).hr.store(E_POINTER, Ordering::Release);
    }
    SetEvent((*this).event);
    0
}

unsafe fn release_unknown(punk: *mut c_void) {
    if punk.is_null() {
        return;
    }
    let vtbl = *(punk as *const *const usize);
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = mem::transmute(*vtbl.add(2));
    release(punk);
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn activate_async() -> Result<ActivateAudioInterfaceAsyncFn> {
    static SLOT: OnceLock<usize> = OnceLock::new();
    let ptr = *SLOT.get_or_init(|| unsafe {
        let lib = LoadLibraryW(wide("mmdevapi.dll").as_ptr());
        if lib.is_null() {
            return 0;
        }
        GetProcAddress(lib, b"ActivateAudioInterfaceAsync\0".as_ptr())
            .map(|far| far as usize)
            .unwrap_or(0)
    });
    if ptr == 0 {
        bail!("ActivateAudioInterfaceAsync is missing; process loopback needs Windows 10 build 20348 or Windows 11");
    }
    Ok(unsafe { mem::transmute(ptr) })
}

unsafe fn activate_process_client(pid: u32, exclude: bool) -> Result<IAudioClient> {
    let activate = activate_async()?;
    let path = wide(r"VAD\Process_Loopback");
    let mut params = ActivationParams {
        activation_type: ACTIVATION_PROCESS_LOOPBACK,
        target_process_id: pid,
        process_loopback_mode: if exclude {
            LOOPBACK_EXCLUDE_TREE
        } else {
            LOOPBACK_INCLUDE_TREE
        },
    };
    let pv = BlobPropVariant {
        vt: VT_BLOB,
        reserved1: 0,
        reserved2: 0,
        reserved3: 0,
        cb_size: mem::size_of::<ActivationParams>() as u32,
        _pad: 0,
        data: (&mut params as *mut ActivationParams).cast(),
    };
    let event = CreateEventW(ptr::null(), 1, 0, ptr::null());
    if event.is_null() {
        bail!("CreateEventW failed for process loopback");
    }
    let handler = Box::into_raw(Box::new(Handler {
        vtbl: &HANDLER_VTBL,
        refs: AtomicI32::new(1),
        event,
        hr: AtomicI32::new(E_FAIL),
        client: AtomicPtr::new(ptr::null_mut()),
    }));
    let mut operation = ptr::null_mut();
    let hr = activate(
        path.as_ptr(),
        &IID_AUDIO_CLIENT,
        &pv,
        handler.cast(),
        &mut operation,
    );
    if hr < 0 {
        handler_release(handler);
        bail!("ActivateAudioInterfaceAsync failed: 0x{hr:08X}");
    }
    let waited = WaitForSingleObject(event, 4000);
    if !operation.is_null() {
        release_unknown(operation);
    }
    if waited != WAIT_OBJECT_0 {
        handler_release(handler);
        bail!("process loopback activation timed out for pid {pid}");
    }
    let code = (*handler).hr.load(Ordering::Acquire);
    let punk = (*handler).client.swap(ptr::null_mut(), Ordering::AcqRel);
    handler_release(handler);
    if code < 0 || punk.is_null() {
        if !punk.is_null() {
            release_unknown(punk);
        }
        bail!("process loopback activation failed for pid {pid}: 0x{code:08X}");
    }
    Ok(IAudioClient::from_raw(punk))
}

struct ProcTap {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    pending: Vec<i16>,
}

impl ProcTap {
    fn open(pid: u32, exclude: bool) -> Result<Self> {
        unsafe {
            let client = activate_process_client(pid, exclude)?;
            // `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` does not implement `GetMixFormat()`
            // (returns `E_NOTIMPL`), so we initialize directly with 48 kHz stereo 16-bit PCM.
            let format = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM,
                nChannels: 2,
                nSamplesPerSec: OPUS_SAMPLE_RATE,
                nAvgBytesPerSec: OPUS_SAMPLE_RATE * 4,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                200_000,
                0,
                &format,
                None,
            )?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Self {
                client,
                capture,
                pending: Vec::new(),
            })
        }
    }

    fn drain(&mut self) -> Result<()> {
        unsafe {
            let mut packet_frames = self.capture.GetNextPacketSize()?;
            while packet_frames > 0 {
                let mut data_ptr = ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;
                self.capture
                    .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)?;
                let silent = (flags & BUFFER_FLAG_SILENT) != 0 || data_ptr.is_null();
                let frames = num_frames as usize;
                if !silent && frames > 0 {
                    let raw = std::slice::from_raw_parts(data_ptr, frames * 4);
                    let stereo = convert_to_stereo_i16(raw, frames, 2, SampleFormat::Int16);
                    self.pending.extend_from_slice(&stereo);
                    if self.pending.len() > MAX_PENDING_SAMPLES {
                        let excess = self.pending.len() - MAX_PENDING_SAMPLES;
                        let drop_count = (excess + (excess & 1)).min(self.pending.len());
                        self.pending.drain(..drop_count);
                    }
                }
                self.capture.ReleaseBuffer(num_frames)?;
                packet_frames = self.capture.GetNextPacketSize()?;
            }
        }
        Ok(())
    }

    fn take_frames(&mut self, frames: usize) -> Vec<i16> {
        let samples = frames * 2;
        if self.pending.len() <= samples {
            return std::mem::take(&mut self.pending);
        }
        let out = self.pending[..samples].to_vec();
        self.pending.drain(..samples);
        out
    }
}

impl Drop for ProcTap {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

pub(crate) fn mix_tracks(tracks: &[&[i16]], frames: usize) -> Vec<i16> {
    let samples = frames.saturating_mul(2);
    let mut acc = vec![0i32; samples];
    for track in tracks {
        let use_samples = (track.len() / 2).min(frames) * 2;
        for (i, sample) in track.iter().take(use_samples).enumerate() {
            acc[i] += i32::from(*sample);
        }
    }
    acc.into_iter()
        .map(|sample| sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16)
        .collect()
}

pub(crate) enum RunOutcome {
    Stopped,
    StreamEnded,
}

pub(crate) fn run_processes(
    mux: &Mux,
    running: &std::sync::atomic::AtomicBool,
    mode: AudioMode,
    pids: &[u32],
    exclude: bool,
    report: &mut dyn FnMut(String),
) -> Result<RunOutcome> {
    let mut taps = Vec::new();
    let mut errors = Vec::new();
    for &pid in pids {
        match ProcTap::open(pid, exclude) {
            Ok(tap) => {
                tracing::info!(pid, exclude, "process audio loopback started");
                taps.push(tap);
            }
            Err(e) => errors.push(format!("pid {pid}: {e:#}")),
        }
    }
    if taps.is_empty() {
        bail!(
            "{}",
            if errors.is_empty() {
                "no process ids to capture".to_string()
            } else {
                errors.join("; ")
            }
        );
    }
    if !errors.is_empty() {
        report(format!(
            "部分进程无法单独回环，其余已投射进程仍会混音：{}",
            errors.join("; ")
        ));
    }
    let mut opus = match mode {
        AudioMode::PcmS16Le => None,
        AudioMode::Opus { bitrate_kbps } => Some(OpusPipeline::new(OPUS_SAMPLE_RATE, bitrate_kbps)?),
    };
    let mut next = Instant::now();
    let mut silent_streak = 0u32;
    while running.load(Ordering::Acquire) {
        let mut i = 0;
        while i < taps.len() {
            if let Err(e) = taps[i].drain() {
                tracing::warn!(error = %e, "process loopback client ended");
                taps.remove(i);
            } else {
                i += 1;
            }
        }
        if taps.is_empty() {
            return Ok(RunOutcome::StreamEnded);
        }
        if Instant::now() >= next {
            let frames = OPUS_FRAME_SAMPLES_PER_CH;
            let chunks: Vec<Vec<i16>> = taps.iter_mut().map(|tap| tap.take_frames(frames)).collect();
            let refs: Vec<&[i16]> = chunks.iter().map(Vec::as_slice).collect();
            let mixed = mix_tracks(&refs, frames);
            let audible = mixed.iter().any(|s| *s != 0);
            if audible {
                silent_streak = 0;
                emit_interleaved_stereo(mux, &mut opus, OPUS_SAMPLE_RATE as u16, &mixed)?;
            } else {
                silent_streak = silent_streak.saturating_add(1);
                if silent_streak <= 2 {
                    emit_interleaved_stereo(mux, &mut opus, OPUS_SAMPLE_RATE as u16, &mixed)?;
                }
            }
            next += Duration::from_millis(20);
            if next + Duration::from_millis(100) < Instant::now() {
                next = Instant::now();
            }
        } else {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    Ok(RunOutcome::Stopped)
}
