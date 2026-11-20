use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
mod tracking;
use crate::{
    capture,
    config::Config,
    input::Injector,
    net::Mux,
    proto::{control_message::Payload, *},
    win32,
};
use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

struct Subscription {
    info: WindowInfo,
    // Explicit subscriptions own themselves; automatically followed windows belong to a root.
    root: u64,
    options: SubscribeWindow,
    ack: u32,
    running: Arc<AtomicBool>,
    key: Arc<AtomicBool>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}

struct Notice {
    id: u64,
    key: Arc<AtomicBool>,
    code: i32,
    message: String,
    done: bool,
}
struct Session {
    config: Arc<Config>,
    codec: i32,
    mux: Arc<Mux>,
    windows: HashMap<u64, Subscription>,
    notices: mpsc::UnboundedSender<Notice>,
    replies: mpsc::Sender<Payload>,
    injector: Injector,
    cursor: Option<CursorShape>,
}
fn updated(info: WindowInfo, ack: u32) -> Payload {
    Payload::WindowUpdated(WindowUpdated {
        window: Some(info),
        ack_resize_seq: ack,
    })
}

impl Session {
    fn send(&self, payload: Payload) -> Result<()> {
        self.replies
            .try_send(payload)
            .map_err(|_| anyhow!("control output congested or disconnected"))
    }
    fn error(&self, id: u64, code: i32, message: impl Into<String>) -> Result<()> {
        self.send(Payload::HostError(HostError {
            window_id: id,
            code,
            message: message.into(),
        }))
    }
    fn start(&mut self, info: WindowInfo, root: u64, options: SubscribeWindow) -> Result<()> {
        let id = info.window_id;
        if self.windows.len() >= 64 && !self.windows.contains_key(&id) {
            return self.error(id, 2, "session window limit reached");
        }
        let ack = self.windows.get(&id).map_or(0, |s| s.ack);
        let running = Arc::new(AtomicBool::new(true));
        let key = Arc::new(AtomicBool::new(true));
        let fps = if options.target_fps == 0 {
            self.config.fps
        } else {
            options.target_fps.min(self.config.fps)
        };
        let rate = if options.max_bitrate_kbps == 0 {
            self.config.bitrate
        } else {
            options.max_bitrate_kbps.min(self.config.bitrate)
        };
        let codec = self.codec;
        let cursor = self.config.capture_cursor;
        let monitor_crop = match options.capture_mode {
            1 => false,
            2 => true,
            _ => self.config.capture_mode == crate::config::CaptureMode::MonitorCrop,
        };
        let notices = self.notices.clone();
        let mux = self.mux.clone();
        self.mux.register(id, info.kind == 3, key.clone());
        let alive = running.clone();
        let force = key.clone();
        let worker = std::thread::Builder::new()
            .name(format!("capture-{id:x}"))
            .spawn(move || {
                let report = |code, message, done| {
                    let _ = notices.send(Notice {
                        id,
                        key: force.clone(),
                        code,
                        message,
                        done,
                    });
                };
                let result = capture::stream(
                    id,
                    fps,
                    rate,
                    codec,
                    cursor,
                    monitor_crop,
                    &alive,
                    &force,
                    |w, h, k, data| mux.push(id, &force, codec as u8, (w, h), k, data),
                    |code, msg| report(code, msg, false),
                );
                if alive.load(Ordering::Acquire) {
                    // Capture reports specific conditions as they occur; use a terminal error on unexpected exit.
                    match result {
                        Err(e) => report(capture::error_code(&e), format!("{e:#}"), true),
                        Ok(()) => report(2, "capture ended".into(), true),
                    }
                }
            });
        if let Err(e) = worker {
            self.mux.end(id, self.codec as u8);
            return self.error(id, 2, e.to_string());
        }
        self.windows.insert(
            id,
            Subscription {
                info: info.clone(),
                root,
                options,
                ack,
                running,
                key,
            },
        );
        self.send(updated(info, ack))?;
        if let Some(shape) = self.cursor.as_ref().filter(|shape| shape.window_id == id) {
            self.send(Payload::CursorShape(shape.clone()))?;
        }
        Ok(())
    }
    fn remove(&mut self, id: u64, closed: bool) -> Result<()> {
        let ids: Vec<_> = self
            .windows
            .iter()
            .filter(|(k, s)| **k == id || s.root == id)
            .map(|(k, _)| *k)
            .collect();
        for child in ids {
            self.windows.remove(&child);
            self.mux.end(child, self.codec as u8);
            if closed || child != id {
                self.send(Payload::WindowClosed(WindowClosed { window_id: child }))?;
            }
        }
        Ok(())
    }
    async fn subscribe(&mut self, req: SubscribeWindow) -> Result<()> {
        let id = req.window_id;
        let list = tokio::task::spawn_blocking(|| win32::list(false)).await??;
        let Some(info) = list.into_iter().find(|w| w.window_id == id) else {
            return self.error(id, 1, "window is no longer available");
        };
        if let Some(old) = self.windows.get(&id) {
            if old.root == id && old.options == req && old.info.pid == info.pid {
                return self.send(updated(info, old.ack));
            }
            self.remove(id, false)?;
        }
        let monitor_crop = match req.capture_mode {
            1 => false,
            2 => true,
            _ => self.config.capture_mode == crate::config::CaptureMode::MonitorCrop,
        };
        self.start(info.clone(), id, req)?;
        if monitor_crop {
            // Bring window to foreground so monitor-crop capture is immediately unobstructed.
            let _ = self
                .injector
                .execute(
                    &info,
                    Payload::FocusWindow(FocusWindow {
                        window_id: id,
                        focused: true,
                    }),
                )
                .await;
        }
        Ok(())
    }
    async fn request(&mut self, msg: Payload) -> Result<()> {
        let target = match msg {
            Payload::Ping(p) => {
                return self.send(Payload::Pong(Pong {
                    echo_timestamp_us: p.echo_timestamp_us,
                }))
            }
            Payload::Pong(_) => return Ok(()),
            Payload::ListWindowsReq(req) => {
                let windows =
                    tokio::task::spawn_blocking(move || win32::list(req.include_icons)).await??;
                return self.send(Payload::ListWindowsRes(ListWindowsResponse { windows }));
            }
            Payload::SubscribeWindow(req) => return self.subscribe(req).await,
            Payload::UnsubscribeWindow(req) => return self.remove(req.window_id, false),
            Payload::RequestKeyFrame(ref req) => req.window_id,
            Payload::ResizeWindow(ref req) => req.window_id,
            Payload::FocusWindow(ref req) => req.window_id,
            Payload::CloseWindow(ref req) => req.window_id,
            Payload::WindowAction(ref req) => req.window_id,
            Payload::MouseEvent(ref req) => req.window_id,
            Payload::KeyEvent(ref req) => req.window_id,
            Payload::TextInput(ref req) => req.window_id,
            _ => return self.error(0, 0, "unexpected client message"),
        };
        let Some(s) = self.windows.get_mut(&target) else {
            return self.error(target, 1, "window is not subscribed");
        };
        if matches!(msg, Payload::RequestKeyFrame(_)) {
            s.key.store(true, Ordering::Release);
            return Ok(());
        }
        match self.injector.execute(&s.info, msg).await {
            Ok(Some(update)) => {
                s.ack = update.ack_resize_seq;
                if let Some(info) = &update.window {
                    s.info = info.clone();
                }
                self.send(Payload::WindowUpdated(update))
            }
            Ok(None) => Ok(()),
            Err(e) => self.error(
                target,
                if e.to_string().contains("ACCESS_DENIED_ELEVATED") {
                    6
                } else {
                    0
                },
                format!("{e:#}"),
            ),
        }
    }
    fn notice(&mut self, n: Notice) -> Result<()> {
        if !self
            .windows
            .get(&n.id)
            .is_some_and(|s| Arc::ptr_eq(&s.key, &n.key))
        {
            return Ok(());
        }
        if n.done {
            tracing::warn!(window_id=n.id, code=n.code, reason=%n.message, "capture worker stopped");
            let gone = n.code == 1 || self.windows[&n.id].root != n.id;
            self.remove(n.id, gone)?;
            if n.code == 1 {
                return Ok(());
            }
        }
        self.error(n.id, n.code, n.message)
    }
}

pub async fn run(
    config: Arc<Config>,
    codec: i32,
    mux: Arc<Mux>,
    mut rx: mpsc::Receiver<Payload>,
    replies: mpsc::Sender<Payload>,
) -> Result<()> {
    let (notices, mut reports) = mpsc::unbounded_channel();
    let injector = Injector::new()?;
    let mut session = Session {
        config,
        codec,
        mux,
        windows: HashMap::new(),
        notices,
        replies,
        injector,
        cursor: None,
    };
    let (events, mut events_rx) = mpsc::channel(128);
    let observer = win32::Observer::new(events)?;
    let mut sweep = tokio::time::interval(Duration::from_secs(1));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut refresh_at = None;
    loop {
        let next =
            refresh_at.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(1));
        tokio::select! {
            Some(n)=reports.recv()=>session.notice(n)?,
            event=events_rx.recv()=>match event {
                Some(win32::Event::Changed(id))=>{
                    if session.windows.contains_key(&id) || session.windows.values().any(|s|s.options.follow_popups) {
                        refresh_at.get_or_insert_with(||tokio::time::Instant::now()+Duration::from_millis(12));
                    }
                }
                Some(win32::Event::Hidden(id))=>{
                    if session.windows.get(&id).is_some_and(|s|s.root!=id) {session.remove(id,true)?;}
                    else if session.windows.contains_key(&id) {refresh_at=Some(tokio::time::Instant::now());}
                }
                Some(win32::Event::Cursor(shape))=>{
                    if session.windows.contains_key(&shape.window_id) {session.send(Payload::CursorShape(shape.clone()))?;}
                    session.cursor=Some(shape);
                }
                Some(win32::Event::Clipboard(sync))=>session.send(Payload::ClipboardSync(sync))?,
                Some(win32::Event::Error(message))=>session.error(0,0,message)?,
                None=>anyhow::bail!("desktop observer disconnected"),
            },
            _=tokio::time::sleep_until(next), if refresh_at.is_some()=>{refresh_at=None;session.refresh().await?;},
            _=sweep.tick()=>{refresh_at=None;session.refresh().await?;},
            msg=rx.recv()=>match msg {
                Some(Payload::ClipboardSync(sync))=>{
                    if let Err(e)=observer.set_clipboard(sync) {session.error(0,0,format!("{e:#}"))?;}
                }
                Some(msg)=>session.request(msg).await?,
                None=>return Ok(()),
            },
        }
    }
}
