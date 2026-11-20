use crate::proto::AppEntry;
use anyhow::{bail, ensure, Result};
use std::{
    collections::{HashMap, HashSet},
    env, fs, mem,
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::Win32::{
    System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
    UI::{
        Shell::{SHGetFileInfoW, ShellExecuteW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON},
        WindowsAndMessaging::{DestroyIcon, SW_SHOWNORMAL},
    },
};

struct StaGuard(bool);
impl StaGuard {
    fn init() -> Self {
        let hr = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        Self(hr >= 0)
    }
}
impl Drop for StaGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn fnv1a_id(canonical_lower: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canonical_lower.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn is_noise_shortcut(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "uninstall",
        "卸载",
        "readme",
        "help",
        "manual",
        "documentation",
        "website",
        "release notes",
        "license",
        "changelog",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

fn collect_shortcuts(dir: &Path, source: &str, depth: usize, out: &mut Vec<(String, String, PathBuf)>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_shortcuts(&path, source, depth - 1, out);
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case("lnk") && !ext.eq_ignore_ascii_case("url") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let name = stem.trim();
        if name.is_empty() || is_noise_shortcut(name) {
            continue;
        }
        out.push((name.to_owned(), source.to_owned(), path));
    }
}

fn file_icon_png(path: &Path) -> Vec<u8> {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let mut sfi: SHFILEINFOW = mem::zeroed();
        let ok = SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut sfi,
            mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok == 0 || sfi.hIcon.is_null() {
            return Vec::new();
        }
        let png = super::image::icon_to_png(sfi.hIcon).unwrap_or_default();
        DestroyIcon(sfi.hIcon);
        png
    }
}

pub fn discover_apps(include_icons: bool) -> (Vec<AppEntry>, HashMap<String, PathBuf>) {
    let _sta = StaGuard::init();
    let mut roots: Vec<(PathBuf, &str)> = Vec::new();
    if let Ok(profile) = env::var("USERPROFILE") {
        roots.push((PathBuf::from(&profile).join("Desktop"), "Desktop"));
    }
    if let Ok(public) = env::var("PUBLIC") {
        roots.push((PathBuf::from(&public).join("Desktop"), "Desktop"));
    }
    if let Ok(appdata) = env::var("APPDATA") {
        roots.push((
            PathBuf::from(&appdata).join("Microsoft\\Windows\\Start Menu\\Programs"),
            "Start Menu",
        ));
    }
    if let Ok(progdata) = env::var("PROGRAMDATA") {
        roots.push((
            PathBuf::from(&progdata).join("Microsoft\\Windows\\Start Menu\\Programs"),
            "Start Menu",
        ));
    }

    let mut raw = Vec::new();
    for (dir, source) in roots {
        collect_shortcuts(&dir, source, 4, &mut raw);
    }

    // Deduplicate by lowercased display name (Desktop items appear first and win over Start Menu).
    let mut seen_names = HashSet::new();
    let mut apps = Vec::new();
    let mut paths = HashMap::new();

    for (name, source, path) in raw {
        let key = name.to_lowercase();
        if !seen_names.insert(key) {
            continue;
        }
        let canonical = path.to_string_lossy().to_lowercase();
        let app_id = fnv1a_id(&canonical);
        let icon_png = if include_icons {
            file_icon_png(&path)
        } else {
            Vec::new()
        };
        paths.insert(app_id.clone(), path);
        apps.push(AppEntry {
            app_id,
            name,
            source,
            icon_png,
        });
    }

    apps.sort_by(|a, b| {
        let rank = |src: &str| if src == "Desktop" { 0 } else { 1 };
        rank(&a.source)
            .cmp(&rank(&b.source))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    (apps, paths)
}

pub fn launch_shortcut(path: &Path) -> Result<()> {
    ensure!(path.exists(), "shortcut no longer exists");
    let _sta = StaGuard::init();
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let res = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            windows_sys::w!("open"),
            wide.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = res as isize;
    if code <= 32 {
        bail!("ShellExecuteW failed with code {code}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_uninstall_and_hashes_stably() {
        assert!(is_noise_shortcut("Uninstall MuMu Player"));
        assert!(is_noise_shortcut("卸载程序"));
        assert!(!is_noise_shortcut("MuMu模拟器12"));
        assert_eq!(fnv1a_id("c:\\a\\b.lnk").len(), 16);
        assert_eq!(fnv1a_id("c:\\a\\b.lnk"), fnv1a_id("c:\\a\\b.lnk"));
        assert_ne!(fnv1a_id("c:\\a\\b.lnk"), fnv1a_id("c:\\a\\c.lnk"));
    }
}
