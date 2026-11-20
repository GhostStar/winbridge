use super::{image::cursor_pixels, Event};
use crate::proto::CursorShape;
use std::{mem, ptr};
use tokio::sync::mpsc::Sender;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub(super) struct Tracker {
    standard: Vec<(HCURSOR, i32)>,
    last: Option<(u64, usize, u32)>,
}
impl Tracker {
    pub fn new() -> Self {
        let resources = [
            IDC_ARROW,
            IDC_IBEAM,
            IDC_HAND,
            IDC_CROSS,
            IDC_WAIT,
            IDC_APPSTARTING,
            IDC_SIZENS,
            IDC_SIZEWE,
            IDC_SIZENWSE,
            IDC_SIZENESW,
            IDC_SIZEALL,
            IDC_NO,
        ];
        let standard = resources
            .into_iter()
            .enumerate()
            .map(|(i, r)| (unsafe { LoadCursorW(ptr::null_mut(), r) }, i as i32 + 1))
            .filter(|(h, _)| !h.is_null())
            .collect();
        Self {
            standard,
            last: None,
        }
    }
    pub fn sample(&mut self, events: &Sender<Event>) {
        unsafe {
            let mut ci: CURSORINFO = mem::zeroed();
            ci.cbSize = mem::size_of::<CURSORINFO>() as u32;
            if GetCursorInfo(&mut ci) == 0 {
                return;
            }
            let target = GetAncestor(WindowFromPoint(ci.ptScreenPos), GA_ROOT) as usize as u64;
            let key = (target, ci.hCursor as usize, ci.flags);
            if self.last == Some(key) {
                return;
            }
            let mut shape = CursorShape {
                window_id: target,
                ..Default::default()
            };
            if ci.flags & CURSOR_SHOWING == 0 {
                shape.cursor_type = 13;
            } else if let Some((_, kind)) = self.standard.iter().find(|(h, _)| *h == ci.hCursor) {
                shape.cursor_type = *kind;
            } else if let Some((rgba, w, h, x, y)) = cursor_pixels(ci.hCursor) {
                shape.cursor_type = 14;
                shape.custom_rgba = rgba;
                shape.width = w;
                shape.height = h;
                shape.hotspot_x = x;
                shape.hotspot_y = y;
            } else {
                shape.cursor_type = 1;
            }
            if events.try_send(Event::Cursor(shape)).is_ok() {
                self.last = Some(key);
            }
        }
    }
}
