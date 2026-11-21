use super::{
    annexb::{BitstreamCodec, ParameterSets},
    video_processor::VideoProcessor,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use std::{
    mem::ManuallyDrop,
    ptr, slice,
    sync::OnceLock,
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, GUID, VARIANT},
    Win32::{
        Foundation::E_NOTIMPL,
        Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D},
        Media::MediaFoundation::*,
        System::Com::CoTaskMemFree,
    },
};

// Initialize Media Foundation once per process. Repeatedly calling MFShutdown/MFStartup
// across reconnects or concurrent capture workers tears down global MF work queues while
// hardware MFT driver threads (NVENC/QSV/AMF) may still be draining, causing native crashes.
struct Runtime;
impl Runtime {
    fn start() -> Result<Self> {
        static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
        let result = INIT.get_or_init(|| {
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string())
        });
        match result {
            Ok(()) => Ok(Self),
            Err(e) => Err(anyhow!("MFStartup failed: {e}")),
        }
    }
}

fn output_subtype(codec: BitstreamCodec) -> GUID {
    match codec {
        BitstreamCodec::H264 => MFVideoFormat_H264,
        BitstreamCodec::Hevc => MFVideoFormat_HEVC,
    }
}

fn codec_label(codec: BitstreamCodec) -> &'static str {
    match codec {
        BitstreamCodec::H264 => "H.264",
        BitstreamCodec::Hevc => "HEVC",
    }
}

fn candidates(codec: BitstreamCodec, hardware: bool) -> Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: output_subtype(codec),
    };
    let flags = if hardware {
        MFT_ENUM_FLAG_HARDWARE
    } else {
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT
    };
    let mut array = ptr::null_mut();
    let mut count = 0;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut array,
            &mut count,
        )?;
        let mut list = Vec::new();
        if !array.is_null() {
            for slot in slice::from_raw_parts_mut(array, count as usize) {
                list.extend(slot.take());
            }
            CoTaskMemFree(Some(array.cast()));
        }
        Ok(list)
    }
}

fn video_type(subtype: &GUID, w: u16, h: u16, fps: u32) -> Result<IMFMediaType> {
    unsafe {
        let mt = MFCreateMediaType()?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(mt)
    }
}

// Field drop order matters for COM/MF teardown: release event generator, codec API,
// and MFT before the DXGI device manager and activation handle.
struct Transform {
    events: Option<IMFMediaEventGenerator>,
    codec: ICodecAPI,
    mft: IMFTransform,
    input: u32,
    output: u32,
    credits: usize,
    gpu: Option<IMFDXGIDeviceManager>,
    activation: IMFActivate,
}
impl Drop for Transform {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if self.gpu.is_some() {
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
            }
            let _ = self.activation.ShutdownObject();
        }
    }
}

impl Transform {
    unsafe fn open(
        activation: &IMFActivate,
        bitstream_codec: BitstreamCodec,
        hardware: bool,
        device: &ID3D11Device,
        w: u16,
        h: u16,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self> {
        let mft: IMFTransform = activation.ActivateObject()?;
        let attrs = mft.GetAttributes().ok();
        let asynchronous = attrs
            .as_ref()
            .and_then(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).ok())
            .unwrap_or(0)
            != 0;
        if let Some(a) = &attrs {
            if asynchronous {
                a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            }
            let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
        }
        let events = if asynchronous {
            Some(mft.cast()?)
        } else {
            None
        };
        let gpu = if hardware {
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("missing DXGI device manager")?;
            manager.ResetDevice(device, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
            Some(manager)
        } else {
            None
        };
        let (mut input_ids, mut output_ids) = ([0u32], [0u32]);
        match mft.GetStreamIDs(&mut input_ids, &mut output_ids) {
            Err(e) if e.code() != E_NOTIMPL => return Err(e.into()),
            _ => {}
        }
        let (input, output) = (input_ids[0], output_ids[0]);
        let codec: ICodecAPI = mft.cast()?;
        let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
        // Disallow B-frames for zero picture-reordering latency.
        let _ = codec.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
        let _ = codec.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate));
        let subtype = output_subtype(bitstream_codec);
        let mt = video_type(&subtype, w, h, fps)?;
        match bitstream_codec {
            BitstreamCodec::H264 => {
                mt.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
            }
            BitstreamCodec::Hevc => {
                let _ = mt.SetUINT32(
                    &MF_MT_MPEG2_PROFILE,
                    eAVEncH265VProfile_Main_420_8.0 as u32,
                );
            }
        }
        mt.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        mft.SetOutputType(output, &mt, 0)?;
        let nv12 = video_type(&MFVideoFormat_NV12, w, h, fps)?;
        nv12.SetUINT32(&MF_MT_DEFAULT_STRIDE, w.into())?;
        mft.SetInputType(input, &nv12, 0)?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        Ok(Self {
            mft,
            codec,
            events,
            input,
            output,
            credits: 0,
            gpu,
            activation: activation.clone(),
        })
    }
}

pub(crate) struct VideoEncoder {
    codec: BitstreamCodec,
    transform: Transform,
    converter: VideoProcessor,
    sets: ParameterSets,
    duration: i64,
    time: i64,
    bytes: u32,
    _runtime: Runtime,
}

impl VideoEncoder {
    pub fn new(
        codec: BitstreamCodec,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        w: u16,
        h: u16,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self> {
        let backends: &[bool] = match codec {
            BitstreamCodec::H264 => &[true, false],
            // Real-time 60fps HEVC requires a hardware MFT (NVENC/QSV/AMF); fall back to H.264 otherwise.
            BitstreamCodec::Hevc => &[true],
        };
        Self::with_backends(codec, device, context, w, h, fps, bitrate_kbps, backends)
    }

    fn with_backends(
        codec: BitstreamCodec,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        w: u16,
        h: u16,
        fps: u32,
        bitrate_kbps: u32,
        hardware_preferences: &[bool],
    ) -> Result<Self> {
        let label = codec_label(codec);
        ensure!(
            fps > 0 && w >= 2 && h >= 2 && w.is_multiple_of(2) && h.is_multiple_of(2),
            "{label} requires positive even dimensions and frame rate"
        );
        let bitrate = bitrate_kbps
            .checked_mul(1000)
            .with_context(|| format!("invalid {label} bitrate"))?;
        let bytes = (u64::from(w) * u64::from(h) * 3 / 2)
            .try_into()
            .context("NV12 frame too large")?;
        let runtime = Runtime::start()?;
        let converter = VideoProcessor::new(device, context, w, h, fps)?;
        let mut last = None;
        for &hw in hardware_preferences {
            let list = match candidates(codec, hw) {
                Ok(list) => list,
                Err(e) => {
                    last = Some(e);
                    continue;
                }
            };
            for activation in list {
                match unsafe { Transform::open(&activation, codec, hw, device, w, h, fps, bitrate) } {
                    Ok(transform) => {
                        tracing::info!(
                            codec = label,
                            hardware = hw,
                            width = w,
                            height = h,
                            "video encoder initialized"
                        );
                        let mut encoder = Self {
                            codec,
                            transform,
                            converter,
                            sets: ParameterSets::new(codec),
                            duration: 10_000_000 / i64::from(fps),
                            time: 0,
                            bytes,
                            _runtime: runtime,
                        };
                        encoder.refresh_config()?;
                        return Ok(encoder);
                    }
                    Err(e) => {
                        unsafe {
                            let _ = activation.ShutdownObject();
                        }
                        tracing::debug!(codec = label, hardware = hw, error = %e, "video encoder unavailable");
                        last = Some(e);
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("no {label} encoder installed")))
            .with_context(|| format!("cannot initialize {label} MFT"))
    }

    fn refresh_config(&mut self) -> Result<()> {
        unsafe {
            let mt = self
                .transform
                .mft
                .GetOutputCurrentType(self.transform.output)?;
            if let Ok(len) = mt.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                ensure!(len < 1024 * 1024, "oversized H.264 sequence header");
                let mut blob = vec![0; len as usize];
                mt.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)?;
                self.sets.config(&blob)?;
            }
        }
        Ok(())
    }

    // Synchronous MFTs signal NEED_MORE_INPUT; async MFTs grant an output event per sample.
    fn output(&mut self) -> Result<Option<(bool, Vec<u8>)>> {
        unsafe {
            for _ in 0..3 {
                let t = &self.transform;
                let info = t.mft.GetOutputStreamInfo(t.output)?;
                let supplies = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                    | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                    as u32;
                let mut out = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: t.output,
                    ..Default::default()
                };
                if info.dwFlags & supplies == 0 {
                    let sample = MFCreateSample()?;
                    let buf = MFCreateAlignedMemoryBuffer(
                        info.cbSize.max(1),
                        info.cbAlignment.saturating_sub(1),
                    )?;
                    sample.AddBuffer(&buf)?;
                    out.pSample = ManuallyDrop::new(Some(sample));
                }
                let mut status = 0;
                let result = t
                    .mft
                    .ProcessOutput(0, slice::from_mut(&mut out), &mut status);
                let sample = ManuallyDrop::take(&mut out.pSample);
                drop(ManuallyDrop::take(&mut out.pEvents));
                match result {
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let format = t.mft.GetOutputAvailableType(t.output, 0)?;
                        ensure!(
                            format.GetGUID(&MF_MT_SUBTYPE)? == output_subtype(self.codec),
                            "unexpected encoder subtype"
                        );
                        t.mft.SetOutputType(t.output, &format, 0)?;
                        self.refresh_config()?;
                        continue;
                    }
                    other => other?,
                }
                if out.dwStatus & MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE.0 as u32
                    == MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE.0 as u32
                {
                    return Ok(None);
                }
                let Some(sample) = sample else {
                    return Ok(None);
                };
                let buf = sample.ConvertToContiguousBuffer()?;
                let (mut data, mut len) = (ptr::null_mut(), 0);
                buf.Lock(&mut data, None, Some(&mut len))?;
                let result = if len == 0 {
                    Ok(None)
                } else if data.is_null() {
                    Err(anyhow!("MFT returned a null bitstream"))
                } else {
                    self.sets
                        .packet(slice::from_raw_parts(data, len as usize))
                        .map(Some)
                };
                buf.Unlock()?;
                return result;
            }
            bail!("H.264 MFT repeatedly changed output format")
        }
    }

    pub fn drain_ready(&mut self) -> Result<Vec<(bool, Vec<u8>)>> {
        let mut frames = Vec::new();
        unsafe {
            if let Some(events) = self.transform.events.clone() {
                loop {
                    let event = match events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                        Ok(event) => event,
                        Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                        Err(e) => return Err(e.into()),
                    };
                    event.GetStatus()?.ok()?;
                    match event.GetType()? as i32 {
                        value if value == METransformNeedInput.0 => self.transform.credits += 1,
                        value if value == METransformHaveOutput.0 => frames.extend(self.output()?),
                        value if value == MEError.0 => bail!("H.264 MFT signaled an error"),
                        _ => {}
                    }
                }
            } else {
                while let Some(frame) = self.output()? {
                    frames.push(frame);
                }
            }
        }
        Ok(frames)
    }

    pub fn encode(&mut self, tex: &ID3D11Texture2D, force: bool) -> Result<Vec<(bool, Vec<u8>)>> {
        let mut frames = self.drain_ready()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.transform.events.is_some() && self.transform.credits == 0 {
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for an H.264 encoder input slot"
            );
            std::thread::sleep(Duration::from_millis(1));
            frames.extend(self.drain_ready()?);
        }
        let nv12 = self.converter.convert(tex)?;
        unsafe {
            let buffer = if self.transform.gpu.is_some() {
                MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &nv12, 0, false)?
            } else {
                let data = self.converter.read(&nv12)?;
                ensure!(
                    data.len() == self.bytes as usize,
                    "incorrect NV12 input size"
                );
                let buf = MFCreateMemoryBuffer(self.bytes)?;
                let mut dst = ptr::null_mut();
                buf.Lock(&mut dst, None, None)?;
                ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
                buf.Unlock()?;
                buf
            };
            buffer.SetCurrentLength(self.bytes)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            sample.SetSampleDuration(self.duration)?;
            if force || self.time == 0 {
                self.transform
                    .codec
                    .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))?;
            }
            self.transform
                .mft
                .ProcessInput(self.transform.input, &sample, 0)?;
            self.transform.credits = self.transform.credits.saturating_sub(1);
            self.time += self.duration;
        }
        // Hardware output normally becomes ready shortly after ProcessInput. Returning
        // it now avoids waiting for the next desktop update on static or low-FPS windows.
        let output_deadline = Instant::now() + Duration::from_millis(4);
        loop {
            frames.extend(self.drain_ready()?);
            if !frames.is_empty() || self.transform.events.is_none() {
                break;
            }
            let remaining = output_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(remaining.min(Duration::from_micros(500)));
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        },
        System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
    };

    #[test]
    #[ignore = "requires an interactive D3D11 device and software H.264 MFT"]
    fn software_encoder_returns_self_contained_idrs() -> Result<()> {
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe {
                    RoUninitialize();
                }
            }
        }
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        let _apartment = Apartment;
        let (mut dev, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut context),
            )?;
        }
        let (device, context) = (dev.unwrap(), context.unwrap());
        let size = 256;
        let pixels = [16u8, 32, 230, 255].repeat((size * size) as usize);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size,
            Height: size,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: size * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        unsafe {
            device.CreateTexture2D(&desc, Some(&init), Some(&mut texture))?;
        }
        let texture = texture.unwrap();
        let mut encoder = VideoEncoder::with_backends(
            BitstreamCodec::H264,
            &device,
            &context,
            size as u16,
            size as u16,
            30,
            2000,
            &[false],
        )?;
        let mut frames = Vec::new();
        for index in 0..16 {
            frames.extend(encoder.encode(&texture, index == 8)?);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while frames.len() < 16 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
            frames.extend(encoder.drain_ready()?);
        }
        ensure!(
            frames.len() == 16,
            "software encoder returned {} frames",
            frames.len()
        );
        for index in [0, 8] {
            let (key, bytes) = &frames[index];
            ensure!(
                *key && bytes.starts_with(&[0, 0, 0, 1, 0x67]),
                "missing SPS on requested IDR"
            );
            let types: Vec<_> = bytes
                .windows(5)
                .filter(|nal| nal[..4] == [0, 0, 0, 1])
                .map(|nal| nal[4] & 31)
                .collect();
            ensure!(
                types.starts_with(&[7, 8]) && types.contains(&5),
                "missing PPS or IDR"
            );
        }
        Ok(())
    }
}
