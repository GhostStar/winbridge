use super::*;
use std::collections::HashSet;

impl Session {
    pub(super) async fn refresh(&mut self) -> Result<()> {
        let now = Instant::now();
        self.pending_launches.retain(|p| p.deadline > now);
        if self.windows.is_empty() && self.pending_launches.is_empty() {
            return Ok(());
        }
        let has_desktop = self.windows.contains_key(&win32::DESKTOP_WINDOW_ID);
        let (list, front, desktop_info) = tokio::task::spawn_blocking(move || -> Result<_> {
            let desktop = if has_desktop {
                win32::primary_desktop_info().ok()
            } else {
                None
            };
            Ok((win32::candidates()?, win32::foreground_id(), desktop))
        })
        .await??;

        if let Some(info) = desktop_info {
            if let Some(sub) = self.windows.get_mut(&win32::DESKTOP_WINDOW_ID) {
                if sub.info != info {
                    let ack = sub.ack;
                    sub.info = info.clone();
                    self.send(updated(info, ack))?;
                }
            }
        }

        let by_id: HashMap<_, _> = list.iter().map(|w| (w.window_id, w)).collect();
        let gone: Vec<_> = self
            .windows
            .iter()
            .filter(|(id, s)| {
                **id != win32::DESKTOP_WINDOW_ID
                    && !by_id.get(id).is_some_and(|w| w.pid == s.info.pid)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.remove(id, true)?;
        }

        for mut info in list.iter().rev().cloned() {
            let id = info.window_id;
            if let Some(s) = self.windows.get(&id) {
                if s.root != id {
                    if s.info.kind == 2 && info.kind == 1 {
                        info.kind = 2;
                    }
                    if let Some(owner) = by_id.get(&s.info.owner_window_id) {
                        relative(&mut info, owner);
                    }
                }
                if s.info != info {
                    let ack = s.ack;
                    self.windows.get_mut(&id).unwrap().info = info.clone();
                    self.send(updated(info, ack))?;
                }
                continue;
            }
            let Some((owner, explicit_owner)) =
                resolve_popup_owner(&info, &by_id, &self.windows, front)
            else {
                continue;
            };
            if let Some(s) = self.windows.get(&owner) {
                let root = s.root;
                let options = s.options;
                if explicit_owner && info.kind == 1 {
                    // Treat explicitly owned cross-process webviews/overlays without WS_POPUP
                    // as owned dialogs so they can receive both pointer and keyboard focus.
                    info.kind = 2;
                }
                relative(&mut info, by_id.get(&owner).copied().unwrap_or(&s.info));
                self.start(info, root, options)?;
            }
        }

        // Auto-subscribe newly opened top-level windows from recent LaunchApp requests.
        if !self.pending_launches.is_empty() {
            let mut remaining = Vec::new();
            let pending = std::mem::take(&mut self.pending_launches);
            for launch in pending {
                let candidate = list.iter().find(|w| {
                    w.kind == 1
                        && !w.is_minimized
                        && w.width >= 120
                        && w.height >= 80
                        && !w.title.is_empty()
                        && !launch.before.contains(&w.window_id)
                        && !self.windows.contains_key(&w.window_id)
                });
                if let Some(win) = candidate.cloned() {
                    let mut opts = launch.options;
                    opts.window_id = win.window_id;
                    let monitor_crop = match opts.capture_mode {
                        1 => false,
                        2 => true,
                        _ => self.config.capture_mode == crate::config::CaptureMode::MonitorCrop,
                    };
                    self.start(win.clone(), win.window_id, opts)?;
                    if monitor_crop {
                        let _ = self
                            .injector
                            .execute(
                                &win,
                                Payload::FocusWindow(FocusWindow {
                                    window_id: win.window_id,
                                    focused: true,
                                }),
                            )
                            .await;
                    }
                } else {
                    remaining.push(launch);
                }
            }
            self.pending_launches = remaining;
        }
        Ok(())
    }
}

fn relative(w: &mut WindowInfo, owner: &WindowInfo) {
    w.owner_window_id = owner.window_id;
    w.offset_x = w.x.saturating_sub(owner.x);
    w.offset_y = w.y.saturating_sub(owner.y);
}

#[derive(Clone, Copy)]
struct SubMeta {
    window_id: u64,
    root: u64,
    pid: u32,
    follow_popups: bool,
}

fn resolve_popup_owner_meta(
    info: &WindowInfo,
    by_id: &HashMap<u64, &WindowInfo>,
    subs: &HashMap<u64, SubMeta>,
    front: u64,
) -> Option<(u64, bool)> {
    // 1. Explicit Win32 GW_OWNER chain pointing into a subscribed window:
    //    allow cross-process owners so embedded WebView2 / CEF browser windows
    //    (e.g. `msedgewebview2.exe` owned by a game) are tracked automatically.
    if info.owner_window_id != 0 {
        let mut owner = info.owner_window_id;
        let mut visited = HashSet::new();
        while owner != 0 && !subs.contains_key(&owner) && visited.insert(owner) {
            owner = by_id.get(&owner).map_or(0, |w| w.owner_window_id);
        }
        if let Some(s) = subs.get(&owner).filter(|s| s.follow_popups) {
            return Some((s.window_id, true));
        }
        return None;
    }

    // 2. Unowned dialogs/popups (owner_window_id == 0): stay strictly within the same PID
    //    so unrelated applications never attach to a subscribed window.
    if !matches!(info.kind, 2 | 3) {
        return None;
    }
    let owner = if let Some(focused) = subs
        .get(&front)
        .filter(|s| s.pid == info.pid && s.follow_popups)
    {
        focused.window_id
    } else if front == info.window_id {
        let mut matches = subs
            .values()
            .filter(|s| s.root == s.window_id && s.pid == info.pid && s.follow_popups);
        let one = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        one.root
    } else {
        return None;
    };
    Some((owner, false))
}

fn resolve_popup_owner(
    info: &WindowInfo,
    by_id: &HashMap<u64, &WindowInfo>,
    windows: &HashMap<u64, Subscription>,
    front: u64,
) -> Option<(u64, bool)> {
    let subs: HashMap<u64, SubMeta> = windows
        .iter()
        .map(|(&id, s)| {
            (
                id,
                SubMeta {
                    window_id: s.info.window_id,
                    root: s.root,
                    pid: s.info.pid,
                    follow_popups: s.options.follow_popups,
                },
            )
        })
        .collect();
    resolve_popup_owner_meta(info, by_id, &subs, front)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(window_id: u64, pid: u32, follow_popups: bool) -> SubMeta {
        SubMeta {
            window_id,
            root: window_id,
            pid,
            follow_popups,
        }
    }

    #[test]
    fn follows_cross_process_explicitly_owned_webview_and_blocks_unowned_other_pid() {
        let game = WindowInfo {
            window_id: 0x100,
            pid: 1000,
            kind: 1,
            ..Default::default()
        };
        let webview_popup = WindowInfo {
            window_id: 0x200,
            owner_window_id: 0x100,
            pid: 2000, // msedgewebview2.exe (different PID!)
            kind: 3,
            ..Default::default()
        };
        let hidden_mid = WindowInfo {
            window_id: 0x201,
            owner_window_id: 0x100,
            pid: 1000,
            kind: 3,
            ..Default::default()
        };
        let multi_hop_webview = WindowInfo {
            window_id: 0x202,
            owner_window_id: 0x201,
            pid: 2000,
            kind: 1, // borderless owned overlay
            ..Default::default()
        };
        let unrelated_popup = WindowInfo {
            window_id: 0x300,
            owner_window_id: 0,
            pid: 3000,
            kind: 3,
            ..Default::default()
        };

        let mut by_id = HashMap::new();
        by_id.insert(game.window_id, &game);
        by_id.insert(webview_popup.window_id, &webview_popup);
        by_id.insert(hidden_mid.window_id, &hidden_mid);
        by_id.insert(multi_hop_webview.window_id, &multi_hop_webview);
        by_id.insert(unrelated_popup.window_id, &unrelated_popup);

        let mut subs = HashMap::new();
        subs.insert(0x100, sub(0x100, 1000, true));

        assert_eq!(
            resolve_popup_owner_meta(&webview_popup, &by_id, &subs, 0x100),
            Some((0x100, true))
        );
        assert_eq!(
            resolve_popup_owner_meta(&multi_hop_webview, &by_id, &subs, 0x100),
            Some((0x100, true))
        );
        // Unowned popup from another PID must NEVER attach.
        assert_eq!(
            resolve_popup_owner_meta(&unrelated_popup, &by_id, &subs, 0x300),
            None
        );

        // Respects follow_popups == false.
        subs.insert(0x100, sub(0x100, 1000, false));
        assert_eq!(
            resolve_popup_owner_meta(&webview_popup, &by_id, &subs, 0x100),
            None
        );
    }
}
