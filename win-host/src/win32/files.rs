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

/// Returns `%USERPROFILE%\Downloads\WinBridge` (resolving Windows Known Folder Downloads).
pub fn default_drop_directory() -> PathBuf {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath},
        };
        unsafe {
            let mut raw: *mut u16 = ptr::null_mut();
            let hr = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, ptr::null_mut(), &mut raw);
            if hr >= 0 && !raw.is_null() {
                let mut len = 0usize;
                while *raw.add(len) != 0 {
                    len += 1;
                }
                let downloads =
                    PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(raw, len)));
                CoTaskMemFree(raw.cast_const().cast());
                return downloads.join("WinBridge");
            }
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
        tracing::info!(path = %final_path.display(), "received dragged file from macOS");
        Ok(Some(final_path))
    }
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
}
