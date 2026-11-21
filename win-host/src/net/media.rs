use anyhow::{ensure, Result};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Notify,
};

use super::micros;

pub const MAX_MEDIA: usize = 64 << 20;
pub const AUDIO_PCM_S16LE: u8 = 10;
pub const AUDIO_OPUS: u8 = 11;
const MAX_AUDIO_QUEUE: usize = 12;
const KEY: u8 = 1;
const EOS: u8 = 4;

pub(crate) struct Frame {
    pub(crate) id: u64,
    time: u64,
    width: u16,
    height: u16,
    codec: u8,
    flags: u8,
    seq: u16,
    pub(crate) bytes: Vec<u8>,
}

impl Frame {
    pub(crate) fn header(&self) -> [u8; 32] {
        let mut h = [0; 32];
        h[..4].copy_from_slice(b"WBMF");
        h[4..12].copy_from_slice(&self.id.to_be_bytes());
        h[12..20].copy_from_slice(&self.time.to_be_bytes());
        h[20..22].copy_from_slice(&self.width.to_be_bytes());
        h[22..24].copy_from_slice(&self.height.to_be_bytes());
        h[24] = self.codec;
        h[25] = self.flags;
        h[26..28].copy_from_slice(&self.seq.to_be_bytes());
        h[28..].copy_from_slice(&(self.bytes.len() as u32).to_be_bytes());
        h
    }

    pub(crate) fn is_eos(&self) -> bool {
        (self.flags & EOS) != 0
    }
}

struct Slot {
    queue: VecDeque<Frame>,
    pending_end: Option<Frame>,
    seq: u16,
    popup: bool,
    wait_idr: bool,
    closed: bool,
    force: Arc<AtomicBool>,
}

#[derive(Default)]
struct Slots {
    by_id: HashMap<u64, Slot>,
    order: VecDeque<u64>,
    prefer_popup: bool,
    audio: VecDeque<Frame>,
    audio_seq: u16,
}

#[derive(Default)]
pub struct Mux {
    slots: Mutex<Slots>,
    wake: Notify,
    audio_wake: Notify,
    video_wake: Notify,
}

impl Mux {
    pub fn register(&self, id: u64, popup: bool, force: Arc<AtomicBool>) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let mut old = slots.by_id.remove(&id);
        let seq = old.as_ref().map_or(0, |s| s.seq);
        if old.is_none() {
            slots.order.push_back(id);
        }
        let pending_end = old.as_mut().and_then(|slot| slot.pending_end.take());
        slots.by_id.insert(
            id,
            Slot {
                queue: VecDeque::new(),
                pending_end,
                seq,
                popup,
                wait_idr: true,
                closed: false,
                force,
            },
        );
    }

    pub fn push(
        &self,
        id: u64,
        token: &Arc<AtomicBool>,
        codec: u8,
        size: (u16, u16),
        key: bool,
        bytes: Vec<u8>,
    ) {
        let (width, height) = size;
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = slots.by_id.get_mut(&id) else {
            return;
        };
        if !Arc::ptr_eq(token, &slot.force) || slot.closed {
            return;
        }
        let seq = slot.seq;
        slot.seq = seq.wrapping_add(1);
        if codec >= 3 || key || slot.queue.len() >= 3 || bytes.len() > MAX_MEDIA {
            slot.queue.clear();
            slot.wait_idr = true;
        }
        if bytes.len() > MAX_MEDIA || slot.wait_idr && !key {
            slot.force.store(true, Ordering::Release);
            return;
        }
        slot.wait_idr = false;
        slot.queue.push_back(Frame {
            id,
            time: micros(),
            width,
            height,
            codec,
            flags: if key { KEY } else { 0 },
            seq,
            bytes,
        });
        self.wake.notify_one();
        self.video_wake.notify_one();
    }

    pub fn end(&self, id: u64, codec: u8) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.by_id.get_mut(&id) {
            slot.closed = true;
            slot.queue.clear();
            slot.pending_end = Some(Frame {
                id,
                time: micros(),
                width: 0,
                height: 0,
                codec,
                flags: EOS,
                seq: slot.seq,
                bytes: Vec::new(),
            });
            slot.seq = slot.seq.wrapping_add(1);
            self.wake.notify_one();
            self.video_wake.notify_one();
        }
    }

    pub fn push_audio(&self, codec: u8, sample_rate: u16, channels: u16, bytes: Vec<u8>) {
        if bytes.is_empty() || bytes.len() > MAX_MEDIA {
            return;
        }
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        while slots.audio.len() >= MAX_AUDIO_QUEUE {
            slots.audio.pop_front();
        }
        let seq = slots.audio_seq;
        slots.audio_seq = seq.wrapping_add(1);
        slots.audio.push_back(Frame {
            id: 0,
            time: micros(),
            width: sample_rate,
            height: channels,
            codec,
            flags: KEY,
            seq,
            bytes,
        });
        self.wake.notify_one();
        self.audio_wake.notify_one();
    }

    pub fn clear_audio(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.audio.clear();
    }

    pub fn backlog(&self, id: u64) -> usize {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.by_id.get(&id).map(|slot| slot.queue.len()).unwrap_or(0)
    }

    pub fn reset(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.audio.clear();
        for slot in slots.by_id.values_mut().filter(|s| !s.closed) {
            slot.queue.clear();
            slot.wait_idr = true;
            slot.force.store(true, Ordering::Release);
        }
    }

    pub(crate) fn request_keyframe(&self, id: u64) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.by_id.get_mut(&id).filter(|s| !s.closed) {
            slot.wait_idr = true;
            slot.force.store(true, Ordering::Release);
        }
    }

    pub(crate) fn pop_audio(&self) -> Option<Frame> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.audio.pop_front()
    }

    pub(crate) fn pop_video(&self) -> Option<(u64, bool, Frame)> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let ready =
            |id: &u64| slots.by_id[id].pending_end.is_some() || !slots.by_id[id].queue.is_empty();
        let preferred = slots
            .prefer_popup
            .then(|| {
                slots
                    .order
                    .iter()
                    .position(|id| ready(id) && slots.by_id[id].popup)
            })
            .flatten();
        let ix = preferred.or_else(|| slots.order.iter().position(ready))?;
        let id = slots.order.remove(ix)?;
        let slot = slots.by_id.get_mut(&id)?;
        let popup = slot.popup;
        let frame = slot.pending_end.take().or_else(|| slot.queue.pop_front());
        if slot.closed {
            slots.by_id.remove(&id);
        } else {
            slots.order.push_back(id);
        }
        slots.prefer_popup = !slots.prefer_popup;
        frame.map(|f| (id, popup, f))
    }

    pub(crate) fn audio_notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.audio_wake.notified()
    }

    pub(crate) fn video_notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.video_wake.notified()
    }

    fn pop(&self) -> Option<Frame> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        // Always prioritize tiny real-time audio packets ahead of large video frames.
        if let Some(audio) = slots.audio.pop_front() {
            return Some(audio);
        }
        // Every second turn gives waiting popups priority. Other turns serve the oldest ready window.
        let ready =
            |id: &u64| slots.by_id[id].pending_end.is_some() || !slots.by_id[id].queue.is_empty();
        let preferred = slots
            .prefer_popup
            .then(|| {
                slots
                    .order
                    .iter()
                    .position(|id| ready(id) && slots.by_id[id].popup)
            })
            .flatten();
        let ix = preferred.or_else(|| slots.order.iter().position(ready))?;
        let id = slots.order.remove(ix)?;
        let slot = slots.by_id.get_mut(&id)?;
        let frame = slot.pending_end.take().or_else(|| slot.queue.pop_front());
        if slot.closed {
            slots.by_id.remove(&id);
        } else {
            slots.order.push_back(id);
        }
        slots.prefer_popup = !slots.prefer_popup;
        frame
    }

    pub async fn send<S: AsyncRead + AsyncWrite + Unpin>(&self, socket: S) -> Result<()> {
        let (mut rx, mut tx) = tokio::io::split(socket);
        let mut probe = [0u8; 1];
        loop {
            let mut wrote = false;
            while let Some(frame) = self.pop() {
                tx.write_all(&frame.header()).await?;
                tx.write_all(&frame.bytes).await?;
                wrote = true;
            }
            if wrote {
                tx.flush().await?;
            }
            tokio::select! {
                _ = self.wake.notified() => {},
                res = rx.read(&mut probe) => {
                    ensure!(res? == 0, "unexpected client media payload");
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interframe_overflow_resumes_at_a_keyframe() {
        let mux = Mux::default();
        let force = Arc::new(AtomicBool::new(false));
        mux.register(42, false, force.clone());
        for i in 0..4 {
            mux.push(42, &force, 1, (16, 16), i == 0, vec![i]);
        }
        assert!(force.load(Ordering::Acquire));
        assert!(mux.pop().is_none());
        mux.push(42, &force, 1, (16, 16), false, vec![4]);
        mux.push(42, &force, 1, (16, 16), true, vec![5]);
        let frame = mux.pop().unwrap();
        assert_eq!((frame.seq, frame.flags, frame.bytes), (5, KEY, vec![5]));
    }

    #[test]
    fn latest_frames_and_popup_priority_preserve_normal_window_progress() {
        let mux = Mux::default();
        let keys: Vec<_> = (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
        for (id, key) in keys.iter().enumerate() {
            mux.register(id as u64, id == 2, key.clone());
        }
        let mut seen = [0; 3];
        for _ in 0..12 {
            for (id, key) in keys.iter().enumerate() {
                for byte in 0..2 {
                    mux.push(id as u64, key, 3, (32, 16), true, vec![byte]);
                }
            }
            let frame = mux.pop().unwrap();
            assert_eq!(frame.bytes, [1]);
            seen[frame.id as usize] += 1;
        }
        assert!(seen[0] > 0 && seen[1] > 0 && seen[2] >= 6);
    }
}
