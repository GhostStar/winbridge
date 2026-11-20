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
            if !matches!(info.kind, 2 | 3) {
                continue;
            }
            let mut owner = info.owner_window_id;
            let mut visited = HashSet::new();
            while owner != 0 && !self.windows.contains_key(&owner) && visited.insert(owner) {
                owner = by_id.get(&owner).map_or(0, |w| w.owner_window_id);
            }
            if info.owner_window_id == 0 {
                if let Some(focused) = self.windows.get(&front).filter(|s| s.info.pid == info.pid) {
                    owner = focused.info.window_id;
                } else if front == id {
                    let mut matches = self.windows.values().filter(|s| {
                        s.root == s.info.window_id
                            && s.info.pid == info.pid
                            && s.options.follow_popups
                    });
                    if let Some(one) = matches.next() {
                        if matches.next().is_none() {
                            owner = one.root;
                        }
                    }
                }
            }
            if let Some(s) = self
                .windows
                .get(&owner)
                .filter(|s| s.info.pid == info.pid && s.options.follow_popups)
            {
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
