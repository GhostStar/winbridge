//! Render Mac microphone PCM into an already-installed virtual playback endpoint.
//! Other apps can record it only if that endpoint is a cable/virtual mic.
//! This module never falls back to the default speakers.

use super::ComGuard;
use anyhow::{anyhow, bail, ensure, Result};
use std::{
    ffi::c_void,
    mem, ptr,
    sync::mpsc,
    thread,
    time::Duration,
};
use windows::{
    core::{Interface, GUID},
    Win32::{
        Media::Audio::{
            eRender, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
            MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
        },
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL},
    },
};

const VT_LPWSTR: u16 = 31;
const AUTOCONVERTPCM: u32 = 0x8000_0000;
const SRC_DEFAULT_QUALITY: u32 = 0x0800_0000;
const FRIENDLY_NAME: GUID = GUID::from_u128(0xA45C254E_DF1C_4EFD_8020_67D146A850E0);

#[repr(C)]
struct PropertyKey {
    fmtid: GUID,
    pid: u32,
}

#[repr(C)]
struct PropVar {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    data: usize,
    extra: u64,
}

pub fn to_stereo_s16(channels: u32, pcm: &[u8]) -> Vec<u8> {
    if channels != 1 {
        return pcm.to_vec();
    }
    let mut out = Vec::with_capacity(pcm.len().saturating_mul(2));
    let mut i = 0;
    while i + 1 < pcm.len() {
        out.extend_from_slice(&pcm[i..i + 2]);
        out.extend_from_slice(&pcm[i..i + 2]);
        i += 2;
    }
    out
}

/// Higher is a better virtual-mic target. 0 means "do not use".
pub fn mic_endpoint_rank(name: &str, preferred: &str) -> i32 {
    let folded = name.trim().to_ascii_lowercase();
    if folded.is_empty() {
        return 0;
    }
    let want = preferred.trim().to_ascii_lowercase();
    if !want.is_empty() {
        if folded == want {
            return 100;
        }
        if folded.contains(&want) {
            return 80;
        }
        return 0;
    }
    if folded.contains("winbridge mic") {
        return 70;
    }
    if folded.contains("cable input") {
        return 60;
    }
    if folded.contains("vb-audio") || folded.contains("vb audio") {
        return 50;
    }
    0
}

unsafe fn friendly_name(device: &IMMDevice) -> Result<String> {
    let raw = device.as_raw();
    let vtbl = *(raw as *const *const usize);
    let open_store: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
        mem::transmute(*vtbl.add(4));
    let mut store = ptr::null_mut();
    let hr = open_store(raw, 0, &mut store);
    ensure!(hr >= 0 && !store.is_null(), "OpenPropertyStore failed: 0x{hr:08X}");
    let sv = *(store as *const *const usize);
    let get_value: unsafe extern "system" fn(*mut c_void, *const PropertyKey, *mut PropVar) -> i32 =
        mem::transmute(*sv.add(5));
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = mem::transmute(*sv.add(2));
    let key = PropertyKey {
        fmtid: FRIENDLY_NAME,
        pid: 14,
    };
    let mut value = PropVar {
        vt: 0,
        reserved1: 0,
        reserved2: 0,
        reserved3: 0,
        data: 0,
        extra: 0,
    };
    let hr = get_value(store, &key, &mut value);
    release(store);
    ensure!(hr >= 0, "friendly name query failed: 0x{hr:08X}");
    if value.vt != VT_LPWSTR || value.data == 0 {
        return Ok(String::new());
    }
    let wide = value.data as *const u16;
    let mut len = 0usize;
    while *wide.add(len) != 0 && len < 512 {
        len += 1;
    }
    let name = String::from_utf16_lossy(std::slice::from_raw_parts(wide, len));
    CoTaskMemFree(Some(wide as *const c_void));
    Ok(name)
}

fn pick_device(preferred: &str) -> Result<(IMMDevice, String)> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;
        let mut best: Option<(IMMDevice, String, i32)> = None;
        for index in 0..count {
            let device = collection.Item(index)?;
            let name = friendly_name(&device).unwrap_or_default();
            let rank = mic_endpoint_rank(&name, preferred);
            if rank == 0 {
                continue;
            }
            let replace = match &best {
                None => true,
                Some((_, _, score)) => rank > *score,
            };
            if replace {
                best = Some((device, name, rank));
            }
        }
        match best {
            Some((device, name, _)) => Ok((device, name)),
            None => bail!(
                "没有匹配的播放设备{}",
                if preferred.trim().is_empty() {
                    "（查找 CABLE Input、VB-Audio 或 WinBridge Mic）".to_string()
                } else {
                    format!("（--mic-device {}）", preferred.trim())
                }
            ),
        }
    }
}

struct Renderer {
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
}

impl Renderer {
    fn open(preferred: &str) -> Result<(Self, String)> {
        unsafe {
            let (device, name) = pick_device(preferred)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let fmt = WAVEFORMATEX {
                wFormatTag: 1,
                nChannels: 2,
                nSamplesPerSec: 48_000,
                nAvgBytesPerSec: 48_000 * 4,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUTOCONVERTPCM | SRC_DEFAULT_QUALITY,
                1_000_000,
                0,
                &fmt,
                None,
            )?;
            let buffer_frames = client.GetBufferSize()?;
            let render: IAudioRenderClient = client.GetService()?;
            client.Start()?;
            Ok((
                Self {
                    client,
                    render,
                    buffer_frames,
                },
                name,
            ))
        }
    }

    fn write(&self, pcm: &[u8]) -> Result<()> {
        let mut offset = 0usize;
        let mut spins = 0u32;
        while offset + 4 <= pcm.len() {
            let padding = unsafe { self.client.GetCurrentPadding()? };
            let free = self.buffer_frames.saturating_sub(padding);
            if free == 0 {
                spins += 1;
                if spins > 40 {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
                continue;
            }
            spins = 0;
            let frames = free.min(((pcm.len() - offset) / 4) as u32);
            unsafe {
                let dest = self.render.GetBuffer(frames)?;
                ensure!(!dest.is_null(), "render buffer is null");
                ptr::copy_nonoverlapping(pcm[offset..].as_ptr(), dest, frames as usize * 4);
                self.render.ReleaseBuffer(frames, 0)?;
            }
            offset += frames as usize * 4;
        }
        Ok(())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

pub struct MicSink {
    tx: Option<mpsc::Sender<Vec<u8>>>,
    thread: Option<thread::JoinHandle<()>>,
    pub endpoint: String,
}

impl MicSink {
    pub fn open(preferred: &str) -> Result<Self> {
        let preferred = preferred.to_string();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let thread = thread::Builder::new()
            .name("mic-uplink".into())
            .spawn(move || {
                let _com = ComGuard::init();
                match Renderer::open(&preferred) {
                    Ok((renderer, name)) => {
                        tracing::info!(endpoint = %name, "virtual mic render started");
                        let _ = ready_tx.send(Ok(name));
                        while let Ok(pcm) = rx.recv() {
                            if let Err(e) = renderer.write(&pcm) {
                                tracing::warn!(error = %e, "virtual mic render stopped");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })?;
        match ready_rx.recv_timeout(Duration::from_secs(4)) {
            Ok(Ok(endpoint)) => Ok(Self {
                tx: Some(tx),
                thread: Some(thread),
                endpoint,
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(anyhow!("opening the virtual microphone timed out")),
        }
    }

    pub fn push(&self, pcm: &[u8]) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let Some(tx) = &self.tx else {
            bail!("virtual microphone renderer is gone");
        };
        tx.send(pcm.to_vec())
            .map_err(|_| anyhow!("virtual microphone renderer is gone"))
    }
}

impl Drop for MicSink {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mic_endpoint_rank;

    #[test]
    fn ranks_virtual_cables_and_ignores_real_speakers() {
        assert_eq!(mic_endpoint_rank("Speakers (Realtek)", ""), 0);
        assert_eq!(mic_endpoint_rank("CABLE Input (VB-Audio Virtual Cable)", ""), 60);
        assert!(mic_endpoint_rank("WinBridge Mic", "") > mic_endpoint_rank("CABLE Input", ""));
        assert_eq!(mic_endpoint_rank("Headphones", "CABLE Input"), 0);
        assert_eq!(mic_endpoint_rank("CABLE Input", "CABLE Input"), 100);
        assert_eq!(mic_endpoint_rank("Something CABLE Input Extra", "cable input"), 80);
        assert_eq!(super::to_stereo_s16(1, &[1, 2, 3, 4]), vec![1, 2, 1, 2, 3, 4, 3, 4]);
    }
}
