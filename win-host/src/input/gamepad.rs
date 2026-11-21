use crate::proto::GamepadState;
use anyhow::{bail, Result};
use std::{
    collections::HashMap,
    ffi::c_void,
};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};

const VIGEM_ERROR_NONE: u32 = 0x2000_0000;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XusbReport {
    pub w_buttons: u16,
    pub b_left_trigger: u8,
    pub b_right_trigger: u8,
    pub s_thumb_lx: i16,
    pub s_thumb_ly: i16,
    pub s_thumb_rx: i16,
    pub s_thumb_ry: i16,
}

impl XusbReport {
    pub fn from_proto(state: &GamepadState) -> Self {
        Self {
            w_buttons: (state.buttons & 0xFFFF) as u16,
            b_left_trigger: state.left_trigger.min(255) as u8,
            b_right_trigger: state.right_trigger.min(255) as u8,
            s_thumb_lx: state.thumb_lx.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            s_thumb_ly: state.thumb_ly.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            s_thumb_rx: state.thumb_rx.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            s_thumb_ry: state.thumb_ry.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        }
    }
}

type FnVigemAlloc = unsafe extern "C" fn() -> *mut c_void;
type FnVigemFree = unsafe extern "C" fn(*mut c_void);
type FnVigemConnect = unsafe extern "C" fn(*mut c_void) -> u32;
type FnVigemDisconnect = unsafe extern "C" fn(*mut c_void);
type FnVigemTargetX360Alloc = unsafe extern "C" fn() -> *mut c_void;
type FnVigemTargetFree = unsafe extern "C" fn(*mut c_void);
type FnVigemTargetAdd = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type FnVigemTargetRemove = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type FnVigemTargetX360Update = unsafe extern "C" fn(*mut c_void, *mut c_void, XusbReport) -> u32;

struct ViGEmApi {
    module: HMODULE,
    client: *mut c_void,
    free: FnVigemFree,
    disconnect: FnVigemDisconnect,
    target_x360_alloc: FnVigemTargetX360Alloc,
    target_free: FnVigemTargetFree,
    target_add: FnVigemTargetAdd,
    target_remove: FnVigemTargetRemove,
    target_x360_update: FnVigemTargetX360Update,
}

unsafe impl Send for ViGEmApi {}

impl ViGEmApi {
    fn try_load() -> Result<Self> {
        unsafe {
            let dll_name: Vec<u16> = "ViGEmClient.dll\0".encode_utf16().collect();
            let module = LoadLibraryW(dll_name.as_ptr());
            if module.is_null() {
                bail!("ViGEmClient.dll not found (install ViGEmBus driver & place ViGEmClient.dll alongside win-host.exe to enable virtual Xbox 360 gamepad passthrough)");
            }

            macro_rules! sym {
                ($name:literal, $ty:ty) => {{
                    let ptr = GetProcAddress(module, concat!($name, "\0").as_ptr());
                    let Some(f) = ptr else {
                        FreeLibrary(module);
                        bail!("ViGEmClient.dll missing export {}", $name);
                    };
                    std::mem::transmute::<_, $ty>(f)
                }};
            }

            let alloc = sym!("vigem_alloc", FnVigemAlloc);
            let free = sym!("vigem_free", FnVigemFree);
            let connect = sym!("vigem_connect", FnVigemConnect);
            let disconnect = sym!("vigem_disconnect", FnVigemDisconnect);
            let target_x360_alloc = sym!("vigem_target_x360_alloc", FnVigemTargetX360Alloc);
            let target_free = sym!("vigem_target_free", FnVigemTargetFree);
            let target_add = sym!("vigem_target_add", FnVigemTargetAdd);
            let target_remove = sym!("vigem_target_remove", FnVigemTargetRemove);
            let target_x360_update = sym!("vigem_target_x360_update", FnVigemTargetX360Update);

            let client = alloc();
            if client.is_null() {
                FreeLibrary(module);
                bail!("vigem_alloc returned null");
            }
            let status = connect(client);
            if status != VIGEM_ERROR_NONE {
                free(client);
                FreeLibrary(module);
                bail!("vigem_connect failed with 0x{status:08X} (is ViGEmBus driver installed?)");
            }
            tracing::info!("ViGEmBus connected for virtual Xbox 360 gamepad passthrough");
            Ok(Self {
                module,
                client,
                free,
                disconnect,
                target_x360_alloc,
                target_free,
                target_add,
                target_remove,
                target_x360_update,
            })
        }
    }
}

impl Drop for ViGEmApi {
    fn drop(&mut self) {
        unsafe {
            (self.disconnect)(self.client);
            (self.free)(self.client);
            FreeLibrary(self.module);
        }
    }
}

/// Optional virtual Xbox 360 controller manager backed by dynamically loaded `ViGEmClient.dll`.
/// Gracefully reports once if ViGEmBus is absent instead of breaking win-host builds or startup.
#[derive(Default)]
pub struct GamepadManager {
    api: Option<ViGEmApi>,
    targets: HashMap<u32, *mut c_void>,
    warned_unavailable: bool,
}

unsafe impl Send for GamepadManager {}

impl GamepadManager {
    pub fn apply(&mut self, state: &GamepadState) -> Result<()> {
        if state.index >= 4 {
            return Ok(());
        }
        if !state.connected {
            self.unplug(state.index);
            return Ok(());
        }
        if self.api.is_none() {
            match ViGEmApi::try_load() {
                Ok(api) => self.api = Some(api),
                Err(e) => {
                    if !self.warned_unavailable {
                        self.warned_unavailable = true;
                        tracing::info!(error = %e, "gamepad passthrough inactive");
                        return Err(e);
                    }
                    return Ok(());
                }
            }
        }
        let api = self.api.as_ref().unwrap();
        let target = if let Some(&existing) = self.targets.get(&state.index) {
            existing
        } else {
            unsafe {
                let pad = (api.target_x360_alloc)();
                if pad.is_null() {
                    bail!("vigem_target_x360_alloc returned null");
                }
                let rc = (api.target_add)(api.client, pad);
                if rc != VIGEM_ERROR_NONE {
                    (api.target_free)(pad);
                    bail!("vigem_target_add failed: 0x{rc:08X}");
                }
                self.targets.insert(state.index, pad);
                pad
            }
        };
        let report = XusbReport::from_proto(state);
        let rc = unsafe { (api.target_x360_update)(api.client, target, report) };
        if rc != VIGEM_ERROR_NONE {
            bail!("vigem_target_x360_update failed: 0x{rc:08X}");
        }
        Ok(())
    }

    fn unplug(&mut self, index: u32) {
        if let (Some(api), Some(target)) = (self.api.as_ref(), self.targets.remove(&index)) {
            unsafe {
                let _ = (api.target_remove)(api.client, target);
                (api.target_free)(target);
            }
        }
    }
}

impl Drop for GamepadManager {
    fn drop(&mut self) {
        let keys: Vec<u32> = self.targets.keys().copied().collect();
        for k in keys {
            self.unplug(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_proto_gamepad_axes_to_xusb_report() {
        let report = XusbReport::from_proto(&GamepadState {
            index: 0,
            connected: true,
            buttons: 0x1000 | 0x4000,
            left_trigger: 300,
            right_trigger: 128,
            thumb_lx: -40_000,
            thumb_ly: 40_000,
            thumb_rx: 1234,
            thumb_ry: -5678,
        });
        assert_eq!(report.w_buttons, 0x5000);
        assert_eq!(report.b_left_trigger, 255);
        assert_eq!(report.b_right_trigger, 128);
        assert_eq!(report.s_thumb_lx, i16::MIN);
        assert_eq!(report.s_thumb_ly, i16::MAX);
        assert_eq!(report.s_thumb_rx, 1234);
        assert_eq!(report.s_thumb_ry, -5678);
    }
}
