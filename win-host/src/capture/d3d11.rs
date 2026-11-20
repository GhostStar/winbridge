use anyhow::{anyhow, ensure, Result};
use windows::{
    core::Interface,
    Graphics::DirectX::Direct3D11::IDirect3DDevice,
    Win32::{
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
            Direct3D11::*,
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                IDXGIDevice,
            },
        },
        System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice,
    },
};

pub(super) struct Graphics {
    pub device: ID3D11Device,
    pub ctx: ID3D11DeviceContext,
    pub runtime: IDirect3DDevice,
}

impl Graphics {
    pub fn new() -> Result<Self> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            let create = |driver, device: &mut Option<_>, ctx: &mut Option<_>| {
                D3D11CreateDevice(
                    None,
                    driver,
                    None,
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(device),
                    None,
                    Some(ctx),
                )
            };
            create(D3D_DRIVER_TYPE_HARDWARE, &mut device, &mut ctx)
                .or_else(|_| create(D3D_DRIVER_TYPE_WARP, &mut device, &mut ctx))?;
            let device = device.ok_or_else(|| anyhow!("D3D11 returned an empty device"))?;
            let ctx = ctx.ok_or_else(|| anyhow!("D3D11 returned an empty context"))?;
            let _ = ctx
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
            let dxgi: IDXGIDevice = device.cast()?;
            let runtime = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            Ok(Self {
                device,
                ctx,
                runtime,
            })
        }
    }

    /// A staging target can be read by the CPU. The default-usage target feeds the GPU encoder.
    pub fn texture(&self, w: u16, h: u16, staging: bool) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w.into(),
            Height: h.into(),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            CPUAccessFlags: if staging {
                D3D11_CPU_ACCESS_READ.0 as u32
            } else {
                0
            },
            BindFlags: if staging {
                0
            } else {
                D3D11_BIND_RENDER_TARGET.0 as u32
            },
            ..Default::default()
        };
        let mut target = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut target))?
        };
        target.ok_or_else(|| anyhow!("D3D11 returned an empty texture"))
    }

    pub fn copy(&self, src: &ID3D11Texture2D, dst: &ID3D11Texture2D, area: D3D11_BOX) {
        unsafe {
            self.ctx
                .CopySubresourceRegion(dst, 0, 0, 0, 0, src, 0, Some(&area))
        };
    }

    pub fn read(&self, staging: &ID3D11Texture2D, w: u16, h: u16) -> Result<Vec<u8>> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.ctx
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?
        };
        let row = w as usize * 4;
        let stride = mapped.RowPitch as usize;
        let pixels = (|| {
            ensure!(
                !mapped.pData.is_null() && stride >= row,
                "invalid D3D11 row pitch"
            );
            let mut pixels = Vec::with_capacity(row * h as usize);
            for y in 0..h as usize {
                // The mapped texture remains valid until Unmap; omit the driver's row padding.
                let source = unsafe { mapped.pData.cast::<u8>().add(y * stride) };
                pixels.extend_from_slice(unsafe { std::slice::from_raw_parts(source, row) });
            }
            Ok(pixels)
        })();
        unsafe { self.ctx.Unmap(staging, 0) };
        pixels
    }
}
