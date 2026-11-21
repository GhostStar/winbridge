//! Minimal pure-Rust PKZIP 2.0 (Store) directory archiver for Windows -> Mac folder pulls.
//! Keeps zero external crate dependencies while supporting UTF-8 file names, CRC-32 integrity,
//! symlink-escape protection, and strict size/entry caps.

use anyhow::{bail, ensure, Context, Result};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAX_DEPTH: usize = 16;
const MAX_ENTRIES: usize = 4096;
const UTF8_FLAG: u16 = 0x0800;

struct CentralEntry {
    name: Vec<u8>,
    crc32: u32,
    size: u32,
    local_header_offset: u32,
}

pub struct TempZip {
    pub path: PathBuf,
    pub total_bytes: u64,
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    crc = !crc;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn collect_files(
    root_canon: &Path,
    dir: &Path,
    rel_prefix: &str,
    depth: usize,
    out: &mut Vec<(String, PathBuf, u64)>,
    total_payload: &mut u64,
    max_bytes: u64,
) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, "folder nesting depth exceeds {MAX_DEPTH}");
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read directory {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let child_path = entry.path();
        let meta = match fs::symlink_metadata(&child_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            // Skip symlinks to prevent escaping the allowed root or directory loops.
            continue;
        }
        let canon = match child_path.canonicalize() {
            Ok(c) if c.starts_with(root_canon) => c,
            _ => continue,
        };
        let rel = if rel_prefix.is_empty() {
            name
        } else {
            format!("{rel_prefix}/{name}")
        };
        if meta.is_dir() {
            collect_files(
                root_canon,
                &canon,
                &rel,
                depth + 1,
                out,
                total_payload,
                max_bytes,
            )?;
        } else if meta.is_file() {
            ensure!(
                out.len() < MAX_ENTRIES,
                "folder contains more than {MAX_ENTRIES} files"
            );
            let len = meta.len();
            *total_payload = total_payload
                .checked_add(len)
                .context("folder total size overflow")?;
            ensure!(
                *total_payload <= max_bytes,
                "folder exceeds maximum transfer size of 512 MiB"
            );
            out.push((rel, canon, len));
        }
    }
    Ok(())
}

/// Packs `dir` into a temporary `.zip` file under the system temp directory.
/// Caller owns cleanup of `TempZip.path`.
pub fn pack_directory_to_temp_zip(dir: &Path, max_bytes: u64) -> Result<TempZip> {
    let root_canon = dir
        .canonicalize()
        .with_context(|| format!("canonicalize {}", dir.display()))?;
    ensure!(root_canon.is_dir(), "not a directory: {}", dir.display());

    let folder_name = root_canon
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "folder".to_owned());

    let mut files = Vec::new();
    let mut total_payload = 0u64;
    collect_files(
        &root_canon,
        &root_canon,
        &folder_name,
        0,
        &mut files,
        &mut total_payload,
        max_bytes,
    )?;

    let tmp_path = std::env::temp_dir().join(format!(
        "winbridge-zip-{}.zip",
        uuid::Uuid::new_v4().simple()
    ));
    let res = write_zip_file(&tmp_path, &folder_name, &files, max_bytes);
    match res {
        Ok(total_bytes) => Ok(TempZip {
            path: tmp_path,
            total_bytes,
        }),
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

fn write_zip_file(
    dst_path: &Path,
    folder_name: &str,
    files: &[(String, PathBuf, u64)],
    max_bytes: u64,
) -> Result<u64> {
    let mut out = File::create(dst_path)
        .with_context(|| format!("create temporary zip {}", dst_path.display()))?;
    let mut central = Vec::with_capacity(files.len().max(1));
    let mut buf = vec![0u8; 64 * 1024];

    // If directory is empty, emit a single directory entry so Finder extracts an empty folder.
    if files.is_empty() {
        let dir_name = format!("{folder_name}/").into_bytes();
        let offset = 0u32;
        write_local_header(&mut out, &dir_name, 0, 0)?;
        central.push(CentralEntry {
            name: dir_name,
            crc32: 0,
            size: 0,
            local_header_offset: offset,
        });
    } else {
        for (rel_name, src_path, _) in files {
            let name_bytes = rel_name.as_bytes().to_vec();
            ensure!(name_bytes.len() <= u16::MAX as usize, "entry path too long");
            let offset_u64 = out.stream_position()?;
            let local_header_offset =
                u32::try_from(offset_u64).context("zip archive exceeds 4 GiB offset limit")?;

            // Write placeholder local header (CRC & size filled after streaming).
            write_local_header(&mut out, &name_bytes, 0, 0)?;
            let mut src = File::open(src_path)
                .with_context(|| format!("open {} for zipping", src_path.display()))?;
            let mut crc = 0u32;
            let mut size = 0u64;
            loop {
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                crc = crc32_update(crc, &buf[..n]);
                size = size
                    .checked_add(n as u64)
                    .context("zip entry size overflow")?;
                out.write_all(&buf[..n])?;
                let current_pos = out.stream_position()?;
                if current_pos > max_bytes {
                    bail!("zipped folder exceeds maximum transfer size of 512 MiB");
                }
            }
            let size_u32 = u32::try_from(size).context("zip entry exceeds 4 GiB")?;
            let end_pos = out.stream_position()?;
            // Patch CRC-32, compressed size, and uncompressed size at local header + 14.
            out.seek(SeekFrom::Start(offset_u64 + 14))?;
            out.write_all(&crc.to_le_bytes())?;
            out.write_all(&size_u32.to_le_bytes())?;
            out.write_all(&size_u32.to_le_bytes())?;
            out.seek(SeekFrom::Start(end_pos))?;

            central.push(CentralEntry {
                name: name_bytes,
                crc32: crc,
                size: size_u32,
                local_header_offset,
            });
        }
    }

    let cd_start = out.stream_position()?;
    let cd_offset = u32::try_from(cd_start).context("central directory offset overflow")?;
    for entry in &central {
        // Central file header signature = 0x02014b50
        out.write_all(&0x0201_4b50u32.to_le_bytes())?;
        out.write_all(&20u16.to_le_bytes())?; // version made by
        out.write_all(&20u16.to_le_bytes())?; // version needed
        out.write_all(&UTF8_FLAG.to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?; // compression method: store
        out.write_all(&0u16.to_le_bytes())?; // mod time
        out.write_all(&0x0021u16.to_le_bytes())?; // mod date (1980-01-01)
        out.write_all(&entry.crc32.to_le_bytes())?;
        out.write_all(&entry.size.to_le_bytes())?;
        out.write_all(&entry.size.to_le_bytes())?;
        out.write_all(&(entry.name.len() as u16).to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?; // extra len
        out.write_all(&0u16.to_le_bytes())?; // comment len
        out.write_all(&0u16.to_le_bytes())?; // disk start
        out.write_all(&0u16.to_le_bytes())?; // internal attrs
        out.write_all(&0u32.to_le_bytes())?; // external attrs
        out.write_all(&entry.local_header_offset.to_le_bytes())?;
        out.write_all(&entry.name)?;
    }
    let cd_end = out.stream_position()?;
    let cd_size = u32::try_from(cd_end - cd_start).context("central directory size overflow")?;
    let count = u16::try_from(central.len()).context("too many zip entries")?;

    // End of central directory record signature = 0x06054b50
    out.write_all(&0x0605_4b50u32.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?; // disk number
    out.write_all(&0u16.to_le_bytes())?; // start disk
    out.write_all(&count.to_le_bytes())?;
    out.write_all(&count.to_le_bytes())?;
    out.write_all(&cd_size.to_le_bytes())?;
    out.write_all(&cd_offset.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?; // comment length
    out.flush()?;

    let total_bytes = out.stream_position()?;
    ensure!(
        total_bytes <= max_bytes,
        "zipped folder exceeds maximum transfer size of 512 MiB"
    );
    Ok(total_bytes)
}

fn write_local_header(out: &mut File, name: &[u8], crc: u32, size: u32) -> Result<()> {
    // Local file header signature = 0x04034b50
    out.write_all(&0x0403_4b50u32.to_le_bytes())?;
    out.write_all(&20u16.to_le_bytes())?; // version needed
    out.write_all(&UTF8_FLAG.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?; // method: store
    out.write_all(&0u16.to_le_bytes())?; // mod time
    out.write_all(&0x0021u16.to_le_bytes())?; // mod date
    out.write_all(&crc.to_le_bytes())?;
    out.write_all(&size.to_le_bytes())?;
    out.write_all(&size.to_le_bytes())?;
    out.write_all(&(name.len() as u16).to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?; // extra len
    out.write_all(name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_standard_ieee_vector() {
        assert_eq!(crc32_update(0, b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn packs_nested_directory_with_valid_zip_signatures() {
        let dir = std::env::temp_dir().join(format!("winbridge-zip-src-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("hello.txt"), b"hello world").unwrap();
        fs::write(dir.join("sub").join("中文.txt"), "你好WinBridge".as_bytes()).unwrap();

        let zip = pack_directory_to_temp_zip(&dir, 10 * 1024 * 1024).unwrap();
        let bytes = fs::read(&zip.path).unwrap();
        assert_eq!(bytes.len() as u64, zip.total_bytes);
        // Starts with PK local header signature
        assert_eq!(&bytes[0..4], &0x0403_4b50u32.to_le_bytes());
        // Ends with EOCD signature (no comment -> last 22 bytes start with 0x06054b50)
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], &0x0605_4b50u32.to_le_bytes());
        let entry_count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]);
        assert_eq!(entry_count, 2);

        let _ = fs::remove_file(&zip.path);
        let _ = fs::remove_dir_all(&dir);
    }
}
