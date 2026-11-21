use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
mod tracking;
use crate::{
    audio::{self, AudioMode, AudioWorker},
    capture,
    config::Config,
    input::{GamepadManager, Injector},
    net::Mux,
    proto::{control_message::Payload, *},
    win32::{self, files::FileReceiver},
};
use anyhow::{anyhow, Result};
use tokio::sync::{mpsc, Semaphore};

/// Host-initiated pull transfer IDs live in the high-bit namespace so they can
/// never collide with client-initiated push transfer IDs (1, 2, 3, ...).
const HOST_PULL_TRANSFER_ID_BASE: u64 = 0x8000_0000_0000_0001;

struct Subscription {
    info: WindowInfo,
    // Explicit subscriptions own themselves; automatically followed windows belong to a root.
    root: u64,
    options: SubscribeWindow,
    ack: u32,
    running: Arc<AtomicBool>,
    key: Arc<AtomicBool>,
    rates: Arc<crate::quality::RateControl>,
    profile_override: Option<i32>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.worker.take() {
            if tokio::runtime::Handle::try_current()
                .is_ok_and(|rt| rt.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
            {
                let _ = tokio::task::block_in_place(|| handle.join());
            } else {
                let _ = handle.join();
            }
        }
    }
}

struct Notice {
    id: u64,
    key: Arc<AtomicBool>,
    code: i32,
    message: String,
    done: bool,
}
pub(super) struct PendingLaunch {
    pub(super) before: HashSet<u64>,
    pub(super) options: SubscribeWindow,
    pub(super) deadline: Instant,
}
struct Session {
    config: Arc<Config>,
    codec: i32,
    mux: Arc<Mux>,
    windows: HashMap<u64, Subscription>,
    apps: HashMap<String, PathBuf>,
    pending_launches: Vec<PendingLaunch>,
    notices: mpsc::UnboundedSender<Notice>,
    replies: mpsc::Sender<Payload>,
    bulk_replies: mpsc::Sender<Payload>,
    pull_cancel: Arc<AtomicBool>,
    pull_slots: Arc<Semaphore>,
    pull_transfer_seq: Arc<AtomicU64>,
    injector: Injector,
    gamepad: GamepadManager,
    files: FileReceiver,
    cursor: Option<CursorShape>,
    audio: Option<AudioWorker>,
    audio_mode: Option<AudioMode>,
    audio_per_process: bool,
    muted_window_ids: HashSet<u64>,
    video_profile: i32,
    video_adaptive: bool,
    video_rtt_ms: u32,
    auto_uac_desktop: bool,
    last_desktop_state: win32::desktop::DesktopState,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.pull_cancel.store(true, Ordering::Release);
        // Stop all capture threads in parallel before their JoinHandles are joined on drop.
        for sub in self.windows.values() {
            sub.running.store(false, Ordering::Release);
        }
    }
}
fn updated(info: WindowInfo, ack: u32) -> Payload {
    Payload::WindowUpdated(WindowUpdated {
        window: Some(info),
        ack_resize_seq: ack,
    })
}

fn send_pulled_files(
    replies: &mpsc::Sender<Payload>,
    bulk_replies: &mpsc::Sender<Payload>,
    cancel: &AtomicBool,
    transfer_seq: &AtomicU64,
    request_id: u64,
    requested: &str,
) -> Result<()> {
    let drop_dir = win32::files::default_drop_directory();
    let roots = win32::files::allowed_read_roots(&drop_dir);
    let is_clipboard = requested.trim() == win32::files::CLIPBOARD_PULL;
    let (paths, skipped) = if is_clipboard {
        let raw = win32::files::clipboard_file_paths()?;
        win32::files::filter_allowed_files(&roots, &raw)?
    } else {
        (
            vec![win32::files::resolve_host_path(&roots, &drop_dir, requested)?],
            Vec::new(),
        )
    };
    if !skipped.is_empty() {
        let _ = replies.try_send(Payload::HostError(HostError {
            window_id: 0,
            code: 0,
            message: format!("剪贴板部分项目未取回：{}", skipped.join("；")),
        }));
    }
    ensure_pull_nonempty(&paths)?;
    for path in paths {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut opened = if is_clipboard {
            win32::files::open_pull_file(&path)?
        } else {
            win32::files::open_pull_target(&path)?
        };
        let transfer_id = transfer_seq.fetch_add(1, Ordering::Relaxed);
        bulk_replies
            .blocking_send(Payload::FileTransferStart(FileTransferStart {
                transfer_id,
                target_window_id: 0,
                file_name: opened.file_name.clone(),
                total_bytes: opened.total_bytes,
                direction: win32::files::FILE_DIRECTION_TO_CLIENT,
                request_id,
            }))
            .map_err(|_| anyhow!("session disconnected"))?;
        let mut offset = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        let stream_res: Result<()> = (|| {
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                let n = opened.read(&mut buf)?;
                let is_last = n == 0 || offset + n as u64 >= opened.total_bytes;
                let data = if n == 0 { Vec::new() } else { buf[..n].to_vec() };
                let chunk_offset = offset;
                offset += n as u64;
                bulk_replies
                    .blocking_send(Payload::FileTransferChunk(FileTransferChunk {
                        transfer_id,
                        offset: chunk_offset,
                        data,
                        is_last,
                    }))
                    .map_err(|_| anyhow!("session disconnected"))?;
                if is_last {
                    break;
                }
            }
            Ok(())
        })();
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        match stream_res {
            Ok(()) => {
                bulk_replies
                    .blocking_send(Payload::FileTransferRes(FileTransferResult {
                        transfer_id,
                        success: true,
                        saved_path: opened.file_name.clone(),
                        message: String::new(),
                    }))
                    .map_err(|_| anyhow!("session disconnected"))?;
            }
            Err(e) => {
                let _ = bulk_replies.blocking_send(Payload::FileTransferRes(FileTransferResult {
                    transfer_id,
                    success: false,
                    saved_path: String::new(),
                    message: format!("{e:#}"),
                }));
                return Ok(());
            }
        }
    }
    Ok(())
}

fn ensure_pull_nonempty(paths: &[std::path::PathBuf]) -> Result<()> {
    if paths.is_empty() {
        Err(anyhow!("nothing to send"))
    } else {
        Ok(())
    }
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
        let (fps, rate) = crate::quality::profile_budget(
            self.video_profile,
            self.config.fps,
            self.config.bitrate,
            options.target_fps,
            options.max_bitrate_kbps,
        );
        let rates = Arc::new(crate::quality::RateControl {
            fps: AtomicU32::new(fps),
            base_bitrate_kbps: AtomicU32::new(rate),
            rtt_ms: AtomicU32::new(self.video_rtt_ms),
            adaptive: AtomicBool::new(self.video_adaptive),
            generation: AtomicU32::new(0),
        });
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
        let rates_thread = rates.clone();
        let mux_backlog = mux.clone();
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
                    &rates_thread,
                    codec,
                    cursor,
                    monitor_crop,
                    &alive,
                    &force,
                    move || mux_backlog.backlog(id),
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
        let handle = match worker {
            Ok(handle) => handle,
            Err(e) => {
                self.mux.end(id, self.codec as u8);
                return self.error(id, 2, e.to_string());
            }
        };
        self.windows.insert(
            id,
            Subscription {
                info: info.clone(),
                root,
                options,
                ack,
                running,
                key,
                rates,
                profile_override: None,
                worker: Some(handle),
            },
        );
        self.send(updated(info, ack))?;
        if let Some(shape) = self.cursor.as_ref().filter(|shape| shape.window_id == id) {
            self.send(Payload::CursorShape(shape.clone()))?;
        }
        let _ = self.sync_audio();
        Ok(())
    }
    fn remove(&mut self, id: u64, closed: bool) -> Result<()> {
        let ids: Vec<_> = self
            .windows
            .iter()
            .filter(|(k, s)| **k == id || s.root == id)
            .map(|(k, _)| *k)
            .collect();
        for &child in &ids {
            if let Some(sub) = self.windows.get(&child) {
                sub.running.store(false, Ordering::Release);
            }
        }
        for child in ids {
            self.windows.remove(&child);
            self.muted_window_ids.remove(&child);
            self.mux.end(child, self.codec as u8);
            if closed || child != id {
                self.send(Payload::WindowClosed(WindowClosed { window_id: child }))?;
            }
        }
        let _ = self.sync_audio();
        Ok(())
    }
    async fn subscribe(&mut self, mut req: SubscribeWindow) -> Result<()> {
        let id = req.window_id;
        let info = if id == win32::DESKTOP_WINDOW_ID {
            req.follow_popups = false;
            match tokio::task::spawn_blocking(win32::primary_desktop_info).await? {
                Ok(info) => info,
                Err(e) => return self.error(id, 4, format!("{e:#}")),
            }
        } else {
            let mut list = tokio::task::spawn_blocking(|| win32::list(false)).await??;
            let Some(mut info) = list.into_iter().find(|w| w.window_id == id) else {
                return self.error(id, 1, "window is no longer available");
            };
            if info.is_minimized {
                let restored =
                    tokio::task::spawn_blocking(move || win32::restore_if_minimized(id)).await?;
                if restored {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    list = tokio::task::spawn_blocking(|| win32::list(false)).await??;
                    if let Some(updated_info) = list.into_iter().find(|w| w.window_id == id) {
                        info = updated_info;
                    }
                }
            }
            info
        };
        if let Some(old) = self.windows.get(&id) {
            if old.root == id && old.options == req && old.info.pid == info.pid {
                return self.send(updated(info, old.ack));
            }
            self.remove(id, false)?;
        }
        let monitor_crop = id != win32::DESKTOP_WINDOW_ID
            && match req.capture_mode {
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
    async fn launch(&mut self, req: LaunchApp) -> Result<()> {
        if !self.apps.contains_key(&req.app_id) {
            let (_, paths) = tokio::task::spawn_blocking(|| win32::discover_apps(false)).await?;
            self.apps = paths;
        }
        let Some(path) = self.apps.get(&req.app_id).cloned() else {
            return self.send(Payload::LaunchAppRes(LaunchAppResult {
                app_id: req.app_id,
                started: false,
                message: "application not found in Start Menu or Desktop".into(),
            }));
        };
        let before: HashSet<u64> = if req.auto_subscribe {
            tokio::task::spawn_blocking(|| win32::list(false))
                .await??
                .into_iter()
                .map(|w| w.window_id)
                .collect()
        } else {
            HashSet::new()
        };
        match tokio::task::spawn_blocking(move || win32::launch_shortcut(&path)).await? {
            Ok(()) => {
                if req.auto_subscribe {
                    self.pending_launches.push(PendingLaunch {
                        before,
                        options: SubscribeWindow {
                            window_id: 0,
                            target_fps: req.target_fps,
                            max_bitrate_kbps: req.max_bitrate_kbps,
                            follow_popups: req.follow_popups,
                            capture_mode: req.capture_mode,
                        },
                        deadline: Instant::now() + Duration::from_secs(25),
                    });
                }
                self.send(Payload::LaunchAppRes(LaunchAppResult {
                    app_id: req.app_id,
                    started: true,
                    message: String::new(),
                }))
            }
            Err(e) => self.send(Payload::LaunchAppRes(LaunchAppResult {
                app_id: req.app_id,
                started: false,
                message: format!("{e:#}"),
            })),
        }
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
            Payload::SubscribeWindow(req) => {
                if req.window_id == win32::DESKTOP_WINDOW_ID {
                    self.auto_uac_desktop = false;
                }
                return self.subscribe(req).await;
            }
            Payload::UnsubscribeWindow(req) => {
                if req.window_id == win32::DESKTOP_WINDOW_ID {
                    self.auto_uac_desktop = false;
                }
                return self.remove(req.window_id, false);
            }
            Payload::ListAppsReq(req) => {
                let (apps, paths) =
                    tokio::task::spawn_blocking(move || win32::discover_apps(req.include_icons))
                        .await?;
                self.apps = paths;
                return self.send(Payload::ListAppsRes(ListAppsResponse { apps }));
            }
            Payload::LaunchApp(req) => return self.launch(req).await,
            Payload::SetAudioConfig(cfg) => {
                self.audio_per_process = cfg.scope == 1;
                self.muted_window_ids = cfg.muted_window_ids.into_iter().collect();
                if cfg.enabled {
                    let desired = if cfg.codec == 11 {
                        AudioMode::Opus {
                            bitrate_kbps: if cfg.bitrate_kbps == 0 {
                                128
                            } else {
                                cfg.bitrate_kbps
                            },
                        }
                    } else {
                        AudioMode::PcmS16Le
                    };
                    self.audio_mode = Some(desired);
                } else {
                    self.audio_mode = None;
                }
                return self.sync_audio();
            }
            Payload::SetVideoConfig(cfg) => return self.apply_video_config(cfg),
            Payload::GamepadState(state) => {
                if let Err(e) = self.gamepad.apply(&state) {
                    let _ = self.error(0, 0, format!("手柄透传提示: {e:#}"));
                }
                return Ok(());
            }
            Payload::FileTransferStart(req) => {
                let id = req.transfer_id;
                if req.direction == win32::files::FILE_DIRECTION_TO_CLIENT {
                    return self.error(0, 0, "client cannot start a host-to-client transfer");
                }
                if let Err(e) = self.files.start(req) {
                    return self.send(Payload::FileTransferRes(FileTransferResult {
                        transfer_id: id,
                        success: false,
                        saved_path: String::new(),
                        message: format!("{e:#}"),
                    }));
                }
                return Ok(());
            }
            Payload::FileTransferChunk(chunk) => {
                if let Some(res) = self.files.chunk(chunk) {
                    return self.send(Payload::FileTransferRes(res));
                }
                return Ok(());
            }
            Payload::ListHostFilesReq(req) => {
                let requested = req.path;
                let requested_for_task = requested.clone();
                let listed = tokio::task::spawn_blocking(move || {
                    let drop_dir = win32::files::default_drop_directory();
                    let roots = win32::files::allowed_read_roots(&drop_dir);
                    let trimmed = requested_for_task.trim();
                    if trimmed.is_empty() || trimmed == win32::files::ROOT_DROP {
                        std::fs::create_dir_all(&drop_dir)?;
                    }
                    let path =
                        win32::files::resolve_host_path(&roots, &drop_dir, &requested_for_task)?;
                    let entries = win32::files::list_directory(&path)?;
                    Ok::<_, anyhow::Error>((win32::files::clean_display_path(&path), entries))
                })
                .await?;
                return match listed {
                    Ok((path, entries)) => self.send(Payload::ListHostFilesRes(ListHostFilesResponse {
                        path,
                        entries: entries
                            .into_iter()
                            .map(|e| HostFileEntry {
                                name: e.name,
                                size_bytes: e.size_bytes,
                                is_dir: e.is_dir,
                            })
                            .collect(),
                        error: String::new(),
                    })),
                    Err(e) => self.send(Payload::ListHostFilesRes(ListHostFilesResponse {
                        path: requested,
                        entries: Vec::new(),
                        error: format!("{e:#}"),
                    })),
                };
            }
            Payload::FilePullReq(req) => {
                let request_id = req.request_id;
                let Ok(permit) = self.pull_slots.clone().try_acquire_owned() else {
                    if request_id != 0 {
                        let _ = self.send(Payload::FileTransferRes(FileTransferResult {
                            transfer_id: request_id,
                            success: false,
                            saved_path: String::new(),
                            message: "已有正在进行的文件取回任务，请等待完成后重试".into(),
                        }));
                        return Ok(());
                    }
                    return self.error(0, 0, "已有正在进行的文件取回任务，请等待完成后重试");
                };
                let replies = self.replies.clone();
                let bulk_replies = self.bulk_replies.clone();
                let cancel = self.pull_cancel.clone();
                let transfer_seq = self.pull_transfer_seq.clone();
                let requested = req.path;
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    if let Err(e) = send_pulled_files(
                        &replies,
                        &bulk_replies,
                        &cancel,
                        &transfer_seq,
                        request_id,
                        &requested,
                    ) {
                        if !cancel.load(Ordering::Acquire) {
                            if request_id != 0 {
                                let _ = replies.try_send(Payload::FileTransferRes(
                                    FileTransferResult {
                                        transfer_id: request_id,
                                        success: false,
                                        saved_path: String::new(),
                                        message: format!("{e:#}"),
                                    },
                                ));
                            } else {
                                let _ = replies.try_send(Payload::HostError(HostError {
                                    window_id: 0,
                                    code: 0,
                                    message: format!("取回文件失败：{e:#}"),
                                }));
                            }
                        }
                    }
                });
                return Ok(());
            }
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

    fn sync_audio(&mut self) -> Result<()> {
        let Some(mode) = self.audio_mode else {
            self.audio = None;
            return Ok(());
        };
        let roots: Vec<(u64, u32)> = self
            .windows
            .values()
            .filter(|sub| sub.root == sub.info.window_id)
            .map(|sub| (sub.info.window_id, sub.info.pid))
            .collect();
        let plan = audio::capture_plan(self.audio_per_process, &roots, &self.muted_window_ids);
        if self
            .audio
            .as_ref()
            .is_some_and(|worker| worker.mode() == mode && worker.plan() == &plan)
        {
            return Ok(());
        }
        self.audio = None;
        match AudioWorker::start(self.mux.clone(), mode, plan, self.replies.clone()) {
            Ok(worker) => {
                self.audio = Some(worker);
                Ok(())
            }
            Err(e) => self.error(0, 0, format!("audio start failed: {e:#}")),
        }
    }

    fn apply_video_config(&mut self, cfg: SetVideoConfig) -> Result<()> {
        if cfg.window_id == 0 {
            if cfg.profile != 0 {
                self.video_profile = cfg.profile;
            }
            self.video_adaptive = cfg.adaptive;
            self.video_rtt_ms = cfg.rtt_ms;
        }
        let ids: Vec<u64> = if cfg.window_id == 0 {
            self.windows.keys().copied().collect()
        } else if self.windows.contains_key(&cfg.window_id) {
            vec![cfg.window_id]
        } else {
            return self.error(cfg.window_id, 1, "window is not subscribed");
        };
        // profile/fps/bitrate all zero is an RTT heartbeat: do not rebuild the budget.
        let touches_budget = cfg.profile != 0 || cfg.target_fps != 0 || cfg.max_bitrate_kbps != 0;
        for id in ids {
            let Some(sub) = self.windows.get_mut(&id) else {
                continue;
            };
            let prev_adaptive = sub.rates.adaptive.swap(cfg.adaptive, Ordering::AcqRel);
            sub.rates.rtt_ms.store(cfg.rtt_ms, Ordering::Release);
            if !touches_budget {
                if prev_adaptive != cfg.adaptive {
                    sub.rates.generation.fetch_add(1, Ordering::Release);
                }
                continue;
            }
            if cfg.window_id != 0 && cfg.profile != 0 {
                sub.profile_override = Some(cfg.profile);
            } else if cfg.window_id == 0 && sub.profile_override.is_some() {
                // Global profile updates do not clobber explicit per-window/per-process overrides.
                sub.rates.generation.fetch_add(1, Ordering::Release);
                continue;
            }
            let profile = if cfg.profile == 0 {
                sub.profile_override.unwrap_or(self.video_profile)
            } else {
                cfg.profile
            };
            let requested_fps = if cfg.target_fps == 0 {
                sub.options.target_fps
            } else {
                cfg.target_fps
            };
            let requested_rate = if cfg.max_bitrate_kbps == 0 {
                sub.options.max_bitrate_kbps
            } else {
                cfg.max_bitrate_kbps
            };
            let (fps, rate) = crate::quality::profile_budget(
                profile,
                self.config.fps,
                self.config.bitrate,
                requested_fps,
                requested_rate,
            );
            sub.rates.fps.store(fps, Ordering::Release);
            sub.rates
                .base_bitrate_kbps
                .store(rate, Ordering::Release);
            sub.rates.generation.fetch_add(1, Ordering::Release);
        }
        Ok(())
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

    async fn poll_secure_desktop(&mut self) -> Result<()> {
        let state = tokio::task::spawn_blocking(win32::desktop::probe_desktop_state).await?;
        win32::desktop::set_secure_desktop_hint(
            state == win32::desktop::DesktopState::SecureAccessible,
        );
        if state == self.last_desktop_state {
            return Ok(());
        }
        tracing::info!(from = ?self.last_desktop_state, to = ?state, "desktop security state changed");
        self.last_desktop_state = state;
        match state {
            win32::desktop::DesktopState::SecureAccessible => {
                if !self.windows.contains_key(&win32::DESKTOP_WINDOW_ID) {
                    self.auto_uac_desktop = true;
                    let req = SubscribeWindow {
                        window_id: win32::DESKTOP_WINDOW_ID,
                        target_fps: 30,
                        max_bitrate_kbps: 0,
                        follow_popups: false,
                        capture_mode: 2,
                    };
                    let _ = self.subscribe(req).await;
                }
            }
            win32::desktop::DesktopState::SecureDenied => {
                let _ = self.error(
                    0,
                    6,
                    "检测到 Windows UAC 安全桌面：请用管理员 PowerShell 运行 win-host.exe --install-service 安装为系统服务后即可在 Mac 上直接看到并点击 UAC",
                );
            }
            win32::desktop::DesktopState::Default => {
                if self.auto_uac_desktop {
                    self.auto_uac_desktop = false;
                    let _ = self.remove(win32::DESKTOP_WINDOW_ID, true);
                }
            }
        }
        Ok(())
    }
}

pub async fn run(
    config: Arc<Config>,
    codec: i32,
    mux: Arc<Mux>,
    mut rx: mpsc::Receiver<Payload>,
    replies: mpsc::Sender<Payload>,
    bulk_replies: mpsc::Sender<Payload>,
) -> Result<()> {
    let (notices, mut reports) = mpsc::unbounded_channel();
    let injector = Injector::new()?;
    let mut session = Session {
        config,
        codec,
        mux,
        windows: HashMap::new(),
        apps: HashMap::new(),
        pending_launches: Vec::new(),
        notices,
        replies,
        bulk_replies,
        pull_cancel: Arc::new(AtomicBool::new(false)),
        pull_slots: Arc::new(Semaphore::new(4)),
        pull_transfer_seq: Arc::new(AtomicU64::new(HOST_PULL_TRANSFER_ID_BASE)),
        injector,
        gamepad: GamepadManager::default(),
        files: FileReceiver::default(),
        cursor: None,
        audio: None,
        audio_mode: None,
        audio_per_process: false,
        muted_window_ids: HashSet::new(),
        video_profile: 1,
        video_adaptive: false,
        video_rtt_ms: 0,
        auto_uac_desktop: false,
        last_desktop_state: win32::desktop::DesktopState::Default,
    };
    let (events, mut events_rx) = mpsc::channel(128);
    let observer = win32::Observer::new(events)?;
    let mut sweep = tokio::time::interval(Duration::from_secs(1));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut uac_poll = tokio::time::interval(Duration::from_millis(200));
    uac_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let notif_watcher = Arc::new(std::sync::Mutex::new(
        win32::notifications::NotificationWatcher::new(),
    ));
    let mut notif_poll = tokio::time::interval(Duration::from_millis(900));
    notif_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut refresh_at = None;
    loop {
        let next =
            refresh_at.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(1));
        tokio::select! {
            Some(n)=reports.recv()=>session.notice(n)?,
            event=events_rx.recv()=>match event {
                Some(win32::Event::Changed(id))=>{
                    if session.windows.contains_key(&id) || session.windows.values().any(|s|s.options.follow_popups) || !session.pending_launches.is_empty() {
                        refresh_at.get_or_insert_with(||tokio::time::Instant::now()+Duration::from_millis(12));
                    }
                }
                Some(win32::Event::Hidden(id))=>{
                    if session.windows.get(&id).is_some_and(|s|s.root!=id) {session.remove(id,true)?;}
                    else if session.windows.contains_key(&id) {refresh_at=Some(tokio::time::Instant::now());}
                }
                Some(win32::Event::Cursor(mut shape))=>{
                    if session.windows.contains_key(&shape.window_id) {
                        session.send(Payload::CursorShape(shape.clone()))?;
                    } else {
                        let fg = win32::foreground_id();
                        if fg != 0 && session.windows.contains_key(&fg) {
                            shape.window_id = fg;
                            session.send(Payload::CursorShape(shape.clone()))?;
                        }
                    }
                    if shape.window_id != win32::DESKTOP_WINDOW_ID
                        && session.windows.contains_key(&win32::DESKTOP_WINDOW_ID)
                    {
                        let mut desktop_shape = shape.clone();
                        desktop_shape.window_id = win32::DESKTOP_WINDOW_ID;
                        session.send(Payload::CursorShape(desktop_shape))?;
                    }
                    session.cursor=Some(shape);
                }
                Some(win32::Event::Clipboard(sync))=>session.send(Payload::ClipboardSync(sync))?,
                Some(win32::Event::ClipboardFiles(notice))=>session.send(Payload::ClipboardFilesNotice(notice))?,
                Some(win32::Event::Error(message))=>session.error(0,0,message)?,
                None=>anyhow::bail!("desktop observer disconnected"),
            },
            _=tokio::time::sleep_until(next), if refresh_at.is_some()=>{refresh_at=None;session.refresh().await?;},
            _=uac_poll.tick()=>{session.poll_secure_desktop().await?;},
            _=notif_poll.tick()=>{
                let watcher = notif_watcher.clone();
                if let Ok(items) = tokio::task::spawn_blocking(move || {
                    watcher.lock().map(|mut w| w.poll()).unwrap_or_default()
                }).await {
                    for note in items {
                        let _ = session.send(Payload::HostNotification(note));
                    }
                }
            },
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
