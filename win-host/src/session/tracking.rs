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
        let (list, front, desktop_info, processes, owners) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let desktop = if has_desktop {
                    win32::primary_desktop_info().ok()
                } else {
                    None
                };
                let list = win32::candidates()?;
                let processes = win32::processes::Snapshot::capture(list.iter().map(|w| w.pid));
                let owners = win32::owner_links(&list);
                Ok((list, win32::foreground_id(), desktop, processes, owners))
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

        for sub in self.windows.values_mut() {
            sub.initial_windows
                .retain(|(id, pid)| by_id.get(id).is_some_and(|w| w.pid == *pid));
        }
        for mut info in list.iter().rev().cloned() {
            let id = info.window_id;
            if let Some(s) = self.windows.get(&id) {
                if s.root != id {
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
            let Some((owner, _explicit_owner)) =
                resolve_popup_owner(&info, &owners, &self.windows, front, &processes)
            else {
                continue;
            };
            if let Some(s) = self.windows.get(&owner) {
                let root = s.root;
                let options = s.options;
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
    related_process: bool,
    new_normal: bool,
}

fn resolve_popup_owner_meta(
    info: &WindowInfo,
    by_id: &HashMap<u64, u64>,
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
            owner = by_id.get(&owner).copied().unwrap_or(0);
        }
        if let Some(s) = subs.get(&owner).filter(|s| s.follow_popups) {
            return Some((s.window_id, true));
        }
        return None;
    }

    // 2. Ownerless normal windows are common for QQ/WeChat image viewers.
    // Follow only a newly appearing foreground window; do not scoop up already
    // open conversations. Other PIDs require verified process-instance ancestry.
    if !matches!(info.kind, 1 | 2 | 3) || info.is_minimized {
        return None;
    }
    let eligible = |s: &&SubMeta| {
        s.follow_popups
            && (s.pid == info.pid || s.related_process)
            && (info.kind != 1 || (s.new_normal && front == info.window_id))
    };
    let owner = if let Some(focused) = subs.get(&front).filter(eligible) {
        focused.window_id
    } else if front == info.window_id {
        let mut matches = subs
            .values()
            .filter(|s| s.root == s.window_id)
            .filter(eligible);
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
    by_id: &HashMap<u64, u64>,
    windows: &HashMap<u64, Subscription>,
    front: u64,
    processes: &win32::processes::Snapshot,
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
                    related_process: processes.same_application(
                        s.info.pid,
                        s.process_started,
                        info.pid,
                    ),
                    new_normal: !s.initial_windows.contains(&(info.window_id, info.pid))
                        && processes.same_application(s.info.pid, s.process_started, info.pid),
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
            related_process: false,
            new_normal: false,
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
        by_id.insert(game.window_id, game.owner_window_id);
        by_id.insert(webview_popup.window_id, webview_popup.owner_window_id);
        by_id.insert(hidden_mid.window_id, hidden_mid.owner_window_id);
        by_id.insert(
            multi_hop_webview.window_id,
            multi_hop_webview.owner_window_id,
        );
        by_id.insert(unrelated_popup.window_id, unrelated_popup.owner_window_id);

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
    #[test]
    fn new_normal_image_viewer_is_followed_but_existing_or_ambiguous_windows_are_not() {
        let info = WindowInfo {
            window_id: 2,
            pid: 10,
            kind: 1,
            ..Default::default()
        };
        let by_id = HashMap::new();
        let mut root = sub(1, 10, true);
        root.new_normal = true;
        let mut subs = HashMap::from([(1, root)]);
        assert_eq!(
            resolve_popup_owner_meta(&info, &by_id, &subs, 2),
            Some((1, false))
        );
        assert_eq!(resolve_popup_owner_meta(&info, &by_id, &subs, 1), None);
        subs.get_mut(&1).unwrap().new_normal = false;
        assert_eq!(resolve_popup_owner_meta(&info, &by_id, &subs, 2), None);
        subs.get_mut(&1).unwrap().new_normal = true;
        subs.insert(
            3,
            SubMeta {
                window_id: 3,
                root: 3,
                ..root
            },
        );
        assert_eq!(resolve_popup_owner_meta(&info, &by_id, &subs, 2), None);
        subs.remove(&3);
        subs.get_mut(&1).unwrap().follow_popups = false;
        assert_eq!(resolve_popup_owner_meta(&info, &by_id, &subs, 2), None);
        subs.get_mut(&1).unwrap().follow_popups = true;
        let helper = WindowInfo {
            pid: 20,
            ..info.clone()
        };
        assert_eq!(resolve_popup_owner_meta(&helper, &by_id, &subs, 2), None);
        subs.get_mut(&1).unwrap().related_process = true;
        assert_eq!(
            resolve_popup_owner_meta(&helper, &by_id, &subs, 2),
            Some((1, false))
        );
        let minimized = WindowInfo {
            is_minimized: true,
            ..info
        };
        assert_eq!(resolve_popup_owner_meta(&minimized, &by_id, &subs, 2), None);
    }
}
