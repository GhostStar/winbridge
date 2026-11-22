//! Local-only process-instance facts for following application-created windows.
use std::{
    collections::{HashMap, HashSet},
    mem,
    path::PathBuf,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE},
    System::{Diagnostics::ToolHelp::*, Threading::*},
};

#[derive(Clone, Debug)]
struct Process {
    parent: u32,
    started: u64,
    image: PathBuf,
}
#[derive(Default)]
pub(crate) struct Snapshot(HashMap<u32, Process>);

pub(crate) fn started(pid: u32) -> Option<u64> {
    inspect(pid, 0).map(|p| p.started)
}
fn inspect(pid: u32, parent: u32) -> Option<Process> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let result = (|| {
            let mut create: FILETIME = mem::zeroed();
            let mut exit = mem::zeroed();
            let mut kernel = mem::zeroed();
            let mut user = mem::zeroed();
            if GetProcessTimes(handle, &mut create, &mut exit, &mut kernel, &mut user) == 0 {
                return None;
            }
            let mut path = vec![0u16; 32768];
            let mut size = path.len() as u32;
            if QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut size) == 0 {
                return None;
            }
            Some(Process {
                parent,
                started: (u64::from(create.dwHighDateTime) << 32) | u64::from(create.dwLowDateTime),
                image: PathBuf::from(
                    String::from_utf16_lossy(&path[..size as usize]).to_lowercase(),
                ),
            })
        })();
        CloseHandle(handle);
        result
    }
}
impl Snapshot {
    pub(crate) fn capture(pids: impl IntoIterator<Item = u32>) -> Self {
        let mut parents = HashMap::new();
        unsafe {
            let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if handle == INVALID_HANDLE_VALUE {
                return Self::default();
            }
            let mut entry: PROCESSENTRY32W = mem::zeroed();
            entry.dwSize = mem::size_of_val(&entry) as u32;
            if Process32FirstW(handle, &mut entry) != 0 {
                loop {
                    parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                    if Process32NextW(handle, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(handle);
        }
        let mut facts = HashMap::new();
        for mut pid in pids {
            let mut visited = HashSet::new();
            for _ in 0..32 {
                if pid == 0 || facts.contains_key(&pid) || !visited.insert(pid) {
                    break;
                }
                let Some(&parent) = parents.get(&pid) else {
                    break;
                };
                let Some(info) = inspect(pid, parent) else {
                    break;
                };
                facts.insert(pid, info);
                pid = parent;
            }
        }
        Self(facts)
    }
    pub(crate) fn same_application(&self, root: u32, instance: u64, candidate: u32) -> bool {
        let Some(base) = self
            .0
            .get(&root)
            .filter(|p| p.started == instance && instance != 0)
        else {
            return false;
        };
        let Some(directory) = base.image.parent() else {
            return false;
        };
        let mut id = candidate;
        let mut visited = HashSet::new();
        for _ in 0..32 {
            if !visited.insert(id) {
                return false;
            }
            let Some(p) = self.0.get(&id) else {
                return false;
            };
            if id == root {
                return p.started == instance;
            }
            // Same folder alone is not sufficient: require a live ancestry chain,
            // with creation times that cannot refer to a reused parent PID.
            if p.image.parent() != Some(directory) {
                return false;
            }
            let Some(parent) = self.0.get(&p.parent) else {
                return false;
            };
            if parent.started > p.started {
                return false;
            }
            id = p.parent;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn follows_verified_helpers_not_siblings_or_reused_parents() {
        let process = |parent, started, path: &str| Process {
            parent,
            started,
            image: PathBuf::from(path),
        };
        let mut snapshot = Snapshot(HashMap::from([
            (1, process(0, 10, "C:/app/chat.exe")),
            (2, process(1, 20, "C:/app/viewer.exe")),
            (3, process(2, 30, "C:/app/viewer.exe")),
            (4, process(0, 15, "C:/app/chat.exe")),
            (5, process(1, 25, "C:/other/browser.exe")),
        ]));
        assert!(snapshot.same_application(1, 10, 1));
        assert!(snapshot.same_application(1, 10, 2));
        assert!(snapshot.same_application(1, 10, 3));
        assert!(!snapshot.same_application(1, 10, 4));
        assert!(!snapshot.same_application(1, 10, 5));
        assert!(!snapshot.same_application(1, 9, 2));
        snapshot.0.get_mut(&2).unwrap().started = 40;
        assert!(!snapshot.same_application(1, 10, 3));
        snapshot.0.get_mut(&2).unwrap().parent = 3;
        assert!(!snapshot.same_application(1, 10, 2));
    }
}
