use crate::proto::{FileTransferChunk, FileTransferResult, FileTransferStart};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    ptr,
};

pub const MAX_TRANSFER_BYTES: u64 = 512 * 1024 * 1024; // 512 MiB
const MAX_CONCURRENT_TRANSFERS: usize = 4;

const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Sanitizes an untrusted client-supplied file name into a safe single path component on Windows.
pub fn sanitize_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "winbridge-file.bin".to_owned();
    }
    if cleaned.len() > 180 {
        cleaned = cleaned.chars().take(180).collect();
    }
    let stem_upper = cleaned
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if RESERVED_WINDOWS_NAMES.contains(&stem_upper.as_str()) {
        cleaned = format!("_{cleaned}");
    }
    cleaned
}

#[cfg(windows)]
fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use windows_sys::Win32::{System::Com::CoTaskMemFree, UI::Shell::SHGetKnownFolderPath};
    unsafe {
        let mut raw: *mut u16 = ptr::null_mut();
        let hr = SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut raw);
        if hr >= 0 && !raw.is_null() {
            let mut len = 0usize;
            while *raw.add(len) != 0 {
                len += 1;
            }
            let path = PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(
                raw, len,
            )));
            CoTaskMemFree(raw.cast_const().cast());
            return Some(path);
        }
    }
    None
}

/// Returns `%USERPROFILE%\Downloads\WinBridge` (resolving Windows Known Folder Downloads).
pub fn default_drop_directory() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(downloads) = known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Downloads) {
            return downloads.join("WinBridge");
        }
    }
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Downloads").join("WinBridge"))
        .unwrap_or_else(|| std::env::temp_dir().join("WinBridge-Drops"))
}

fn unique_destination(dir: &Path, safe_name: &str) -> PathBuf {
    let candidate = dir.join(safe_name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(safe_name);
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(safe_name);
    let ext = p.extension().and_then(|s| s.to_str());
    for n in 1..10_000u32 {
        let name = match ext {
            Some(e) if !e.is_empty() => format!("{stem} ({n}).{e}"),
            _ => format!("{stem} ({n})"),
        };
        let next = dir.join(name);
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!("{safe_name}.new"))
}

struct ActiveTransfer {
    file: File,
    part_path: PathBuf,
    final_path: PathBuf,
    expected_bytes: u64,
    received_bytes: u64,
}

impl Drop for ActiveTransfer {
    fn drop(&mut self) {
        if self.part_path.exists() {
            let _ = fs::remove_file(&self.part_path);
        }
    }
}

pub struct FileReceiver {
    base_dir: PathBuf,
    active: HashMap<u64, ActiveTransfer>,
}

impl Default for FileReceiver {
    fn default() -> Self {
        Self::new(default_drop_directory())
    }
}

impl FileReceiver {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            active: HashMap::new(),
        }
    }

    pub fn start(&mut self, req: FileTransferStart) -> Result<()> {
        ensure!(
            self.active.len() < MAX_CONCURRENT_TRANSFERS,
            "too many concurrent file transfers"
        );
        ensure!(
            req.total_bytes <= MAX_TRANSFER_BYTES,
            "file exceeds maximum transfer size of 512 MiB"
        );
        fs::create_dir_all(&self.base_dir)
            .with_context(|| format!("create drop directory {}", self.base_dir.display()))?;
        let safe_name = sanitize_file_name(&req.file_name);
        let final_path = unique_destination(&self.base_dir, &safe_name);
        let part_path = self.base_dir.join(format!(
            ".winbridge-{}-{}.part",
            req.transfer_id,
            uuid::Uuid::new_v4().simple()
        ));
        let file = File::create(&part_path)
            .with_context(|| format!("create temp file {}", part_path.display()))?;
        self.active.insert(
            req.transfer_id,
            ActiveTransfer {
                file,
                part_path,
                final_path,
                expected_bytes: req.total_bytes,
                received_bytes: 0,
            },
        );
        Ok(())
    }

    pub fn chunk(&mut self, chunk: FileTransferChunk) -> Option<FileTransferResult> {
        let id = chunk.transfer_id;
        let res = self.apply_chunk(chunk);
        match res {
            Ok(Some(saved)) => Some(FileTransferResult {
                transfer_id: id,
                success: true,
                saved_path: saved.to_string_lossy().into_owned(),
                message: String::new(),
            }),
            Ok(None) => None,
            Err(e) => {
                self.active.remove(&id);
                Some(FileTransferResult {
                    transfer_id: id,
                    success: false,
                    saved_path: String::new(),
                    message: format!("{e:#}"),
                })
            }
        }
    }

    fn apply_chunk(&mut self, chunk: FileTransferChunk) -> Result<Option<PathBuf>> {
        let id = chunk.transfer_id;
        {
            let transfer = self
                .active
                .get_mut(&id)
                .context("unknown or expired transfer_id")?;
            ensure!(
                chunk.offset == transfer.received_bytes,
                "out-of-order file chunk (expected offset {}, got {})",
                transfer.received_bytes,
                chunk.offset
            );
            let next_total = transfer
                .received_bytes
                .checked_add(chunk.data.len() as u64)
                .context("transfer size overflow")?;
            ensure!(
                next_total <= transfer.expected_bytes && next_total <= MAX_TRANSFER_BYTES,
                "received bytes exceed declared file size"
            );
            if !chunk.data.is_empty() {
                transfer.file.write_all(&chunk.data)?;
                transfer.received_bytes = next_total;
            }
            if !chunk.is_last {
                return Ok(None);
            }
            ensure!(
                transfer.received_bytes == transfer.expected_bytes,
                "incomplete file transfer ({} of {} bytes)",
                transfer.received_bytes,
                transfer.expected_bytes
            );
            transfer.file.flush()?;
        }

        let Some(mut transfer) = self.active.remove(&id) else {
            bail!("transfer missing during finalization");
        };
        // Close handle before renaming on Windows.
        let part = transfer.part_path.clone();
        let final_path = unique_destination(
            &self.base_dir,
            transfer
                .final_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("winbridge-file.bin"),
        );
        // Clear part_path before drop so Drop doesn't remove the finalized file.
        transfer.part_path = PathBuf::new();
        drop(transfer);
        fs::rename(&part, &final_path).with_context(|| {
            let _ = fs::remove_file(&part);
            format!("finalize dropped file to {}", final_path.display())
        })?;
        if let Err(e) = set_clipboard_file_paths(&[&final_path]) {
            tracing::debug!(error = %e, "could not publish dropped file to Windows clipboard");
        }
        tracing::info!(path = %final_path.display(), "received dragged file from macOS");
        Ok(Some(final_path))
    }
}

pub const FILE_DIRECTION_TO_CLIENT: u32 = 2;
pub const CLIPBOARD_PULL: &str = "clipboard:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFileEntryInfo {
    pub name: String,
    pub size_bytes: u64,
    pub is_dir: bool,
}

pub struct OpenedPull {
    pub file_name: String,
    pub total_bytes: u64,
    pub file: File,
}

/// Roots the client may read: the drop folder plus the user Downloads, Desktop, and Documents.
pub fn allowed_read_roots(drop_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![drop_dir.to_path_buf()];
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
        };
        for id in [&FOLDERID_Downloads, &FOLDERID_Desktop, &FOLDERID_Documents] {
            if let Some(dir) = known_folder(id) {
                if !roots.contains(&dir) {
                    roots.push(dir);
                }
            }
        }
    }
    if let Some(home) = user_home() {
        for name in ["Downloads", "Desktop", "Documents"] {
            let dir = home.join(name);
            if !roots.contains(&dir) {
                roots.push(dir);
            }
        }
    }
    roots
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Canonicalizes `candidate` and rejects anything that resolves outside `roots`.
pub fn canonicalize_in_roots(roots: &[PathBuf], candidate: &Path) -> Result<PathBuf> {
    if !candidate.exists() {
        bail!("path does not exist: {}", candidate.display());
    }
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("canonicalize {}", candidate.display()))?;
    let allowed = roots.iter().any(|root| {
        let Ok(root_canon) = root.canonicalize() else {
            return false;
        };
        canonical.starts_with(&root_canon)
    });
    ensure!(
        allowed,
        "path is outside the allowed folders (Downloads, Desktop, Documents, WinBridge drop)"
    );
    Ok(canonical)
}

/// Resolves a client path and rejects anything that escapes `roots` after canonicalization.
pub fn resolve_host_path(roots: &[PathBuf], drop_dir: &Path, requested: &str) -> Result<PathBuf> {
    let requested = requested.trim();
    ensure!(requested != CLIPBOARD_PULL, "clipboard pull is not a filesystem path");
    let candidate = if requested.is_empty() {
        drop_dir.to_path_buf()
    } else {
        let raw = Path::new(requested);
        if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            ensure!(
                !requested.split(['/', '\\']).any(|part| part == ".."),
                "relative path may not contain '..'"
            );
            drop_dir.join(raw)
        }
    };
    canonicalize_in_roots(roots, &candidate)
}

/// Filters clipboard paths to regular files that canonicalize inside `roots`.
/// Returns `(allowed_files, skipped_reasons)`.
pub fn filter_allowed_files(
    roots: &[PathBuf],
    raw_paths: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let mut allowed = Vec::new();
    let mut skipped = Vec::new();
    for raw in raw_paths {
        match canonicalize_in_roots(roots, raw) {
            Ok(canon) => match fs::metadata(&canon) {
                Ok(meta) if meta.is_file() => allowed.push(canon),
                Ok(_) => skipped.push(format!(
                    "{}（已跳过文件夹）",
                    raw.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| raw.display().to_string())
                )),
                Err(e) => skipped.push(format!("{}（无法读取：{e}）", raw.display())),
            },
            Err(_) => skipped.push(format!(
                "{}（不在允许目录 Downloads/Desktop/Documents 内）",
                raw.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| raw.display().to_string())
            )),
        }
    }
    ensure!(
        !allowed.is_empty(),
        "剪贴板中没有允许取回的文件{}",
        if skipped.is_empty() {
            String::new()
        } else {
            format!("：{}", skipped.join("；"))
        }
    );
    Ok((allowed, skipped))
}

pub fn list_directory(path: &Path) -> Result<Vec<HostFileEntryInfo>> {
    let mut entries = Vec::new();
    let rd = fs::read_dir(path).with_context(|| format!("read {}", path.display()))?;
    for item in rd {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let meta = item.metadata()?;
        entries.push(HostFileEntryInfo {
            name,
            size_bytes: if meta.is_file() { meta.len() } else { 0 },
            is_dir: meta.is_dir(),
        });
    }
    entries.sort_by(|a, b| a.is_dir.cmp(&b.is_dir).reverse().then(a.name.cmp(&b.name)));
    Ok(entries)
}

pub fn open_pull_file(path: &Path) -> Result<OpenedPull> {
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    ensure!(meta.is_file(), "not a file: {}", path.display());
    ensure!(
        meta.len() <= MAX_TRANSFER_BYTES,
        "file exceeds maximum transfer size of 512 MiB"
    );
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "winbridge-file.bin".into());
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(OpenedPull {
        file_name: sanitize_file_name(&file_name),
        total_bytes: meta.len(),
        file,
    })
}

#[cfg(windows)]
pub fn clipboard_file_paths() -> Result<Vec<PathBuf>> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
        UI::Shell::{DragQueryFileW, HDROP},
    };
    const CF_HDROP: u32 = 15;
    unsafe {
        ensure!(OpenClipboard(std::ptr::null_mut()) != 0, "OpenClipboard failed");
        let hdrop = GetClipboardData(CF_HDROP);
        if hdrop.is_null() {
            CloseClipboard();
            bail!("Windows clipboard has no files");
        }
        let drop = hdrop as HDROP;
        let count = DragQueryFileW(drop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(drop, i, buf.as_mut_ptr(), buf.len() as u32);
            buf.truncate(len as usize);
            paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
        }
        CloseClipboard();
        ensure!(!paths.is_empty(), "Windows clipboard has no files");
        Ok(paths)
    }
}

/// Builds a Win32 `DROPFILES` (UTF-16LE, double-NUL terminated) memory block for `CF_HDROP`.
pub fn build_dropfiles_blob(paths: &[&Path]) -> Vec<u8> {
    // DROPFILES struct: pFiles(u32=20), pt.x(i32=0), pt.y(i32=0), fNC(i32=0), fWide(i32=1)
    let mut out = Vec::with_capacity(20 + paths.len() * 64);
    out.extend_from_slice(&20u32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&1i32.to_le_bytes());
    for path in paths {
        let text = path.as_os_str().to_string_lossy();
        for unit in text.encode_utf16() {
            if unit != 0 {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(windows)]
pub fn set_clipboard_file_paths(paths: &[&Path]) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::GlobalFree,
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
    };
    const CF_HDROP: u32 = 15;
    ensure!(!paths.is_empty(), "no file paths to place on clipboard");
    let blob = build_dropfiles_blob(paths);
    unsafe {
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        ensure!(opened, "OpenClipboard failed");
        let hmem = GlobalAlloc(GMEM_MOVEABLE, blob.len());
        if hmem.is_null() {
            CloseClipboard();
            bail!("GlobalAlloc failed for CF_HDROP");
        }
        let dst = GlobalLock(hmem);
        if dst.is_null() {
            GlobalFree(hmem);
            CloseClipboard();
            bail!("GlobalLock failed for CF_HDROP");
        }
        ptr::copy_nonoverlapping(blob.as_ptr(), dst.cast(), blob.len());
        GlobalUnlock(hmem);
        if EmptyClipboard() == 0 || SetClipboardData(CF_HDROP, hmem).is_null() {
            GlobalFree(hmem);
            CloseClipboard();
            bail!("SetClipboardData(CF_HDROP) failed");
        }
        CloseClipboard();
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_clipboard_file_paths(_paths: &[&Path]) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn clipboard_file_paths() -> Result<Vec<PathBuf>> {
    bail!("clipboard file pull is only available on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_path_traversal_and_reserved_windows_names() {
        assert_eq!(sanitize_file_name("../../evil/notes.txt"), "notes.txt");
        assert_eq!(sanitize_file_name(r"C:\Windows\System32\cmd.exe"), "cmd.exe");
        assert_eq!(sanitize_file_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_file_name("hello:world?.png"), "hello_world_.png");
        assert_eq!(sanitize_file_name("..."), "winbridge-file.bin");
    }

    #[test]
    fn receives_chunked_file_and_deduplicates_name() {
        let dir = std::env::temp_dir().join(format!("winbridge-file-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut rx = FileReceiver::new(dir.clone());

        for id in [1u64, 2u64] {
            rx.start(FileTransferStart {
                transfer_id: id,
                target_window_id: 0,
                file_name: "demo.txt".into(),
                total_bytes: 11,
                direction: 0,
            })
            .unwrap();
            assert!(rx
                .chunk(FileTransferChunk {
                    transfer_id: id,
                    offset: 0,
                    data: b"hello ".to_vec(),
                    is_last: false,
                })
                .is_none());
            let res = rx
                .chunk(FileTransferChunk {
                    transfer_id: id,
                    offset: 6,
                    data: b"world".to_vec(),
                    is_last: true,
                })
                .unwrap();
            assert!(res.success, "{}", res.message);
            assert_eq!(fs::read_to_string(&res.saved_path).unwrap(), "hello world");
        }
        assert!(dir.join("demo.txt").exists());
        assert!(dir.join("demo (1).txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_paths_outside_allowed_roots_and_lists_a_child() {
        let root = std::env::temp_dir().join(format!("winbridge-pull-{}", std::process::id()));
        let drop_dir = root.join("WinBridge");
        let outside = std::env::temp_dir().join(format!("winbridge-pull-out-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&drop_dir).unwrap();
        fs::create_dir_all(drop_dir.join("notes")).unwrap();
        fs::write(drop_dir.join("notes").join("a.txt"), b"abc").unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"nope").unwrap();

        let roots = vec![drop_dir.clone()];
        let listed = resolve_host_path(&roots, &drop_dir, "notes").unwrap();
        assert!(listed.ends_with("notes"));
        let entries = list_directory(&listed).unwrap();
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size_bytes, 3);
        assert!(!entries[0].is_dir);

        let escaped = outside.join("secret.txt");
        let err = resolve_host_path(&roots, &drop_dir, escaped.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("outside"), "{err}");
        assert!(resolve_host_path(&roots, &drop_dir, "notes/../../secret.txt").is_err());

        let opened = open_pull_file(&listed.join("a.txt")).unwrap();
        assert_eq!(opened.file_name, "a.txt");
        assert_eq!(opened.total_bytes, 3);

        // Clipboard file filter must enforce the same allowed-roots jail and skip directories.
        let (allowed, skipped) = filter_allowed_files(
            &roots,
            &[listed.join("a.txt"), listed.clone(), escaped.clone()],
        )
        .unwrap();
        assert_eq!(allowed.len(), 1);
        assert!(allowed[0].ends_with("a.txt"));
        assert_eq!(skipped.len(), 2);
        assert!(filter_allowed_files(&roots, &[escaped]).is_err());

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn builds_valid_utf16_dropfiles_blob() {
        let p1 = Path::new(r"C:\Users\me\Downloads\WinBridge\a.png");
        let blob = build_dropfiles_blob(&[p1]);
        assert_eq!(u32::from_le_bytes(blob[0..4].try_into().unwrap()), 20);
        assert_eq!(i32::from_le_bytes(blob[16..20].try_into().unwrap()), 1);
        let units: Vec<u16> = blob[20..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        // Ends with double NUL terminator
        assert_eq!(units[units.len() - 2..], [0, 0]);
        let decoded = String::from_utf16(&units[..units.len() - 2]).unwrap();
        assert_eq!(decoded, r"C:\Users\me\Downloads\WinBridge\a.png");
    }
}
