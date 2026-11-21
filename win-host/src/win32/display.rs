//! Monitor enumeration and optional mode switch for an already-installed virtual adapter.
//! WinBridge does not ship or load a kernel driver.

use super::window::DESKTOP_WINDOW_ID;
use crate::proto::{MonitorInfo, WindowInfo};
use anyhow::{bail, ensure, Result};
use std::{mem, ptr};
use windows_sys::Win32::{
    Foundation::{BOOL, LPARAM, RECT},
    Graphics::Gdi::{
        ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW,
        CDS_UPDATEREGISTRY, DEVMODEW, DISPLAY_DEVICEW, HDC, HMONITOR, MONITORINFOF_PRIMARY,
    },
};

pub const MONITOR_ID_BASE: u64 = 0xFFFF_FFFF_FFFF_0100;
const MONITOR_ID_LIMIT: u64 = MONITOR_ID_BASE + 64;

pub fn is_monitor_id(id: u64) -> bool {
    (MONITOR_ID_BASE..MONITOR_ID_LIMIT).contains(&id)
}

pub fn monitor_index(id: u64) -> Option<usize> {
    is_monitor_id(id).then_some((id - MONITOR_ID_BASE) as usize)
}

pub fn looks_virtual(name: &str) -> bool {
    let folded = name.to_ascii_lowercase();
    ["virtual", "indirect", "idd", "usbmmid", "parsec", "virtualdisplay"]
        .iter()
        .any(|needle| folded.contains(needle))
}

#[repr(C)]
struct MonitorInfoExW {
    cb_size: u32,
    rc_monitor: RECT,
    rc_work: RECT,
    flags: u32,
    device: [u16; 32],
}

struct RawMonitor {
    handle: isize,
    device_name: String,
    friendly_name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    primary: bool,
    virtual_adapter: bool,
}

fn utf16_buf(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

fn adapter_names() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut index = 0u32;
    loop {
        let mut device: DISPLAY_DEVICEW = unsafe { mem::zeroed() };
        device.cb = mem::size_of::<DISPLAY_DEVICEW>() as u32;
        let ok = unsafe { EnumDisplayDevicesW(ptr::null(), index, &mut device, 0) };
        if ok == 0 {
            break;
        }
        let name = utf16_buf(&device.DeviceName);
        let text = utf16_buf(&device.DeviceString);
        if !name.is_empty() {
            out.push((name, text));
        }
        index += 1;
        if index > 64 {
            break;
        }
    }
    out
}

fn enumerate() -> Result<Vec<RawMonitor>> {
    let adapters = adapter_names();
    let mut handles: Vec<isize> = Vec::new();
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        if !monitor.is_null() {
            let list = &mut *(data as *mut Vec<isize>);
            list.push(monitor as isize);
        }
        1
    }
    let ok = unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(each),
            &mut handles as *mut Vec<isize> as LPARAM,
        )
    };
    ensure!(ok != 0, "DISPLAY_DISCONNECTED: EnumDisplayMonitors failed");
    let mut monitors = Vec::new();
    for handle in handles {
        let mut info: MonitorInfoExW = unsafe { mem::zeroed() };
        info.cb_size = mem::size_of::<MonitorInfoExW>() as u32;
        let got = unsafe {
            GetMonitorInfoW(
                handle as HMONITOR,
                (&mut info as *mut MonitorInfoExW).cast(),
            )
        };
        if got == 0 {
            continue;
        }
        let device_name = utf16_buf(&info.device);
        let friendly = adapters
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&device_name))
            .map(|(_, text)| text.clone())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| device_name.clone());
        let width = info.rc_monitor.right.saturating_sub(info.rc_monitor.left).max(0) as u32;
        let height = info.rc_monitor.bottom.saturating_sub(info.rc_monitor.top).max(0) as u32;
        let virtual_adapter = looks_virtual(&device_name) || looks_virtual(&friendly);
        monitors.push(RawMonitor {
            handle,
            device_name,
            friendly_name: friendly,
            x: info.rc_monitor.left,
            y: info.rc_monitor.top,
            width,
            height,
            primary: (info.flags & MONITORINFOF_PRIMARY) != 0,
            virtual_adapter,
        });
    }
    ensure!(
        !monitors.is_empty(),
        "DISPLAY_DISCONNECTED: no monitors are attached"
    );
    Ok(monitors)
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    Ok(enumerate()?
        .into_iter()
        .enumerate()
        .map(|(index, mon)| MonitorInfo {
            monitor_id: MONITOR_ID_BASE + index as u64,
            device_name: mon.device_name,
            friendly_name: mon.friendly_name,
            width: mon.width,
            height: mon.height,
            x: mon.x,
            y: mon.y,
            primary: mon.primary,
            virtual_adapter: mon.virtual_adapter,
        })
        .collect())
}

pub fn monitor_window_info(id: u64) -> Result<WindowInfo> {
    let index = monitor_index(id).ok_or_else(|| anyhow_id(id))?;
    let monitors = enumerate()?;
    let mon = monitors
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("DISPLAY_DISCONNECTED: monitor index {index} is gone"))?;
    let label = if mon.virtual_adapter {
        "虚拟显示器"
    } else if mon.primary {
        "主显示器"
    } else {
        "显示器"
    };
    Ok(WindowInfo {
        window_id: id,
        owner_window_id: 0,
        pid: 0,
        process_name: if mon.virtual_adapter {
            "virtual-display".into()
        } else {
            "monitor".into()
        },
        title: format!(
            "{label} {} ({}x{})",
            mon.friendly_name, mon.width, mon.height
        ),
        kind: 1,
        x: mon.x,
        y: mon.y,
        width: mon.width,
        height: mon.height,
        dpi: 96,
        is_resizable: false,
        ..Default::default()
    })
}

fn anyhow_id(id: u64) -> anyhow::Error {
    anyhow::anyhow!("DISPLAY_DISCONNECTED: 0x{id:X} is not a monitor id")
}

pub fn monitor_handle(index: usize) -> Result<isize> {
    let monitors = enumerate()?;
    monitors.get(index).map(|mon| mon.handle).ok_or_else(|| {
        anyhow::anyhow!("DISPLAY_DISCONNECTED: monitor index {index} is not active")
    })
}

pub fn apply_virtual_mode(width: u32, height: u32, hz: u32) -> Result<String> {
    let width = if width == 0 { 2560 } else { width };
    let height = if height == 0 { 1440 } else { height };
    let hz = if hz == 0 { 60 } else { hz };
    ensure!(width >= 640 && height >= 480, "virtual display mode is too small");
    ensure!(width <= 7680 && height <= 4320, "virtual display mode is too large");
    let monitors = enumerate()?;
    let Some(mon) = monitors.iter().find(|mon| mon.virtual_adapter) else {
        bail!(
            "DISPLAY_DISCONNECTED: 没有已安装的虚拟显示器。WinBridge 不附带内核驱动，请先安装 IddCx / Indirect Display / Parsec / usbmmidd 一类的虚拟适配器后再设置 {}x{}。",
            width, height
        );
    };
    let mut mode: DEVMODEW = unsafe { mem::zeroed() };
    mode.dmSize = mem::size_of::<DEVMODEW>() as u16;
    mode.dmFields = 0x0004_0000 | 0x0008_0000 | 0x0010_0000 | 0x0040_0000;
    mode.dmBitsPerPel = 32;
    mode.dmPelsWidth = width;
    mode.dmPelsHeight = height;
    mode.dmDisplayFrequency = hz;
    let name: Vec<u16> = mon
        .device_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let rc = unsafe {
        ChangeDisplaySettingsExW(
            name.as_ptr(),
            &mode,
            ptr::null_mut(),
            CDS_UPDATEREGISTRY,
            ptr::null(),
        )
    };
    ensure!(
        rc == 0,
        "DISPLAY_DISCONNECTED: {} 拒绝 {}x{}@{}Hz（ChangeDisplaySettingsEx={rc}）。确认该虚拟适配器支持这个模式。",
        mon.friendly_name,
        width,
        height,
        hz
    );
    Ok(format!(
        "已将 {} ({}) 设为 {width}x{height}@{hz}Hz",
        mon.friendly_name, mon.device_name
    ))
}

#[allow(dead_code)]
fn _desktop_id_stays_primary() -> u64 {
    DESKTOP_WINDOW_ID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_adapter_names_match_known_drivers_only() {
        assert!(looks_virtual("Virtual Display Driver"));
        assert!(looks_virtual(r"\\.\DISPLAY2 (Indirect Display)"));
        assert!(looks_virtual("IddCx Sample"));
        assert!(looks_virtual("usbmmidd"));
        assert!(looks_virtual("Parsec Virtual Display Adapter"));
        assert!(!looks_virtual("Generic PnP Monitor"));
        assert!(!looks_virtual(r"\\.\DISPLAY1"));
        assert!(!looks_virtual("Dell U2720Q"));
    }

    #[test]
    fn monitor_ids_do_not_alias_the_primary_desktop() {
        assert!(!is_monitor_id(DESKTOP_WINDOW_ID));
        assert_eq!(monitor_index(MONITOR_ID_BASE), Some(0));
        assert_eq!(monitor_index(MONITOR_ID_BASE + 3), Some(3));
        assert_eq!(monitor_index(MONITOR_ID_LIMIT), None);
    }
}
