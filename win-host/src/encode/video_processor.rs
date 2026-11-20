use anyhow::{ensure, Context, Result};
use std::mem::ManuallyDrop;
use windows::{
    core::Interface,
    Win32::{
        Foundation::BOOL,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
    },
};

pub struct VideoProcessor {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_ctx: ID3D11VideoContext,
    enumeration: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    width: u16,
    height: u16,
    staging: Option<ID3D11Texture2D>,
}

impl VideoProcessor {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u16,
        height: u16,
        fps: u32,
    ) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && width.is_multiple_of(2) && height.is_multiple_of(2),
            "NV12 requires positive even dimensions"
        );
        let video: ID3D11VideoDevice = device.cast().context("D3D11 video device")?;
        let video_ctx: ID3D11VideoContext = context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: u32::from(width),
            InputHeight: u32::from(height),
            OutputFrameRate: rate,
            OutputWidth: u32::from(width),
            OutputHeight: u32::from(height),
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumeration = video.CreateVideoProcessorEnumerator(&desc)?;
            let input = enumeration.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)?;
            let output = enumeration.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?;
            ensure!(
                input & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0
                    && output & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 != 0,
                "display driver lacks BGRA-to-NV12 video processing support"
            );
            let processor = video.CreateVideoProcessor(&enumeration, 0)?;
            video_ctx.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_ctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let color_ctx: ID3D11VideoContext1 = video_ctx.cast()?;
            color_ctx.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            color_ctx.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            Ok(Self {
                device: device.clone(),
                context: context.clone(),
                video,
                video_ctx,
                enumeration,
                processor,
                width,
                height,
                staging: None,
            })
        }
    }

    fn texture(&self, staging: bool) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width.into(),
            Height: self.height.into(),
            ArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            BindFlags: if staging {
                0
            } else {
                D3D11_BIND_RENDER_TARGET.0 as u32
            },
            CPUAccessFlags: if staging {
                D3D11_CPU_ACCESS_READ.0 as u32
            } else {
                0
            },
            ..Default::default()
        };
        let mut out = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut out))? };
        out.context("D3D11 returned an empty NV12 texture")
    }

    pub fn convert(&self, bgra: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        // MF may retain an input sample after ProcessInput returns. Give it separate
        // backing storage per sample so it can never see the next frame's pixels.
        let nv12 = self.texture(false)?;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        unsafe {
            let mut input = None;
            self.video.CreateVideoProcessorInputView(
                bgra,
                &self.enumeration,
                &input_desc,
                Some(&mut input),
            )?;
            ensure!(input.is_some(), "D3D11 returned an empty input view");
            let mut output = None;
            self.video.CreateVideoProcessorOutputView(
                &nv12,
                &self.enumeration,
                &output_desc,
                Some(&mut output),
            )?;
            let output = output.context("D3D11 returned an empty output view")?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                pInputSurface: ManuallyDrop::new(input),
                ..Default::default()
            };
            let result = self.video_ctx.VideoProcessorBlt(
                &self.processor,
                &output,
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
        }
        Ok(nv12)
    }

    pub fn read(&mut self, nv12: &ID3D11Texture2D) -> Result<Vec<u8>> {
        if self.staging.is_none() {
            self.staging = Some(self.texture(true)?);
        }
        let staging = self.staging.as_ref().unwrap();
        let w = self.width as usize;
        let rows = self.height as usize * 3 / 2;
        unsafe {
            self.context.CopyResource(staging, nv12);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let pixels = (|| {
                let pitch = mapped.RowPitch as usize;
                ensure!(
                    !mapped.pData.is_null() && pitch >= w,
                    "invalid NV12 row pitch"
                );
                let mut packed = Vec::with_capacity(w * rows);
                // NV12 maps the UV plane immediately after height padded luma rows.
                for y in 0..rows {
                    packed.extend_from_slice(std::slice::from_raw_parts(
                        mapped.pData.cast::<u8>().add(y * pitch),
                        w,
                    ));
                }
                Ok(packed)
            })();
            self.context.Unmap(staging, 0);
            pixels
        }
    }
}
