//! Windows Action Center toast notification watcher (`wpndatabase.db`).
//!
//! Reads a private snapshot copy of `%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db`
//! (plus its `-wal` journal) using Windows' inbox `winsqlite3.dll` via runtime `LoadLibraryW`,
//! requiring zero extra crates and zero MSIX/sparse package identity.

use crate::proto::{HostNotification, WindowInfo};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static WATCHER_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DbStamp {
    db_len: u64,
    db_mtime_ns: u128,
    wal_len: u64,
    wal_mtime_ns: u128,
}

fn file_stamp(path: &Path) -> (u64, u128) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, 0);
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    (meta.len(), mtime)
}

#[cfg(windows)]
fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

// ============================================================================
// Pure-Rust Toast XML Parser, PrimaryId Humanizer & Window Matcher
// ============================================================================

/// Unescapes standard XML named & numeric character entities.
pub fn decode_xml_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            let entity = &after[..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => {
                    entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Extracts `(title, body)` from a Windows `<toast>...</toast>` XML payload.
pub fn parse_toast_xml(xml: &str) -> (String, String) {
    let lower = xml.to_ascii_lowercase();
    let mut texts: Vec<String> = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = lower[cursor..].find("<text") {
        let tag_start = cursor + rel_start;
        let after_name = tag_start + 5;
        // Make sure `<text` is not a prefix of another tag like `<texture`.
        let boundary_ok = lower
            .as_bytes()
            .get(after_name)
            .is_some_and(|&b| b == b'>' || b == b'/' || b.is_ascii_whitespace());
        if !boundary_ok {
            cursor = after_name;
            continue;
        }
        let Some(rel_gt) = lower[after_name..].find('>') else {
            break;
        };
        let open_end = after_name + rel_gt;
        // Skip self-closing `<text .../>`.
        if xml[tag_start..open_end].trim_end().ends_with('/') {
            cursor = open_end + 1;
            continue;
        }
        let content_start = open_end + 1;
        let Some(rel_close) = lower[content_start..].find("</text>") else {
            break;
        };
        let content_end = content_start + rel_close;
        let raw_inner = &xml[content_start..content_end];
        let cleaned = decode_xml_entities(raw_inner)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !cleaned.is_empty() {
            texts.push(cleaned);
        }
        cursor = content_end + "</text>".len();
    }

    if texts.is_empty() {
        return (String::new(), String::new());
    }
    let title = texts.remove(0);
    let body = texts.join("\n");
    (title, body)
}

fn friendly_app_alias(stem: &str) -> Option<&'static str> {
    match stem.to_ascii_lowercase().as_str() {
        "wechat" | "weixin" => Some("微信"),
        "qq" => Some("QQ"),
        "tim" => Some("TIM"),
        "feishu" | "lark" => Some("飞书"),
        "dingtalk" => Some("钉钉"),
        "wxwork" => Some("企业微信"),
        "discord" => Some("Discord"),
        "slack" => Some("Slack"),
        "telegram" => Some("Telegram"),
        "whatsapp" | "whatsappdesktop" => Some("WhatsApp"),
        "teams" | "msteams" => Some("Microsoft Teams"),
        "msedge" => Some("Microsoft Edge"),
        "chrome" => Some("Google Chrome"),
        "firefox" => Some("Firefox"),
        "code" => Some("VS Code"),
        "steam" | "steamwebhelper" => Some("Steam"),
        "outlook" | "olk" => Some("Outlook"),
        "explorer" => Some("Windows 资源管理器"),
        _ => None,
    }
}

/// Converts a Windows `NotificationHandler.PrimaryId` (EXE path, AUMID, or Squirrel ID)
/// into `(display_app_name, best_effort_process_name)`.
pub fn humanize_primary_id(primary_id: &str) -> (String, String) {
    let trimmed = primary_id.trim();
    if trimmed.is_empty() {
        return ("Windows".to_owned(), String::new());
    }

    // 1. Executable path or shell-folder GUID path: `...\WeChat\WeChat.exe`
    if trimmed.contains('\\') || trimmed.to_ascii_lowercase().ends_with(".exe") {
        let tail = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
        let stem = tail
            .strip_suffix(".exe")
            .or_else(|| tail.strip_suffix(".EXE"))
            .unwrap_or(tail);
        let app = friendly_app_alias(stem)
            .map(str::to_owned)
            .unwrap_or_else(|| stem.to_owned());
        let proc_name = if tail.to_ascii_lowercase().ends_with(".exe") {
            tail.to_owned()
        } else {
            format!("{stem}.exe")
        };
        return (app, proc_name);
    }

    // 2. UWP / MSIX AUMID: `5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App`
    let before_bang = trimmed.split('!').next().unwrap_or(trimmed);
    let before_pub = before_bang.split('_').next().unwrap_or(before_bang);
    let last_seg = before_pub
        .rsplit('.')
        .find(|seg| !seg.is_empty() && !seg.eq_ignore_ascii_case("app"))
        .unwrap_or(before_pub);

    let app = friendly_app_alias(last_seg)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            last_seg
                .strip_suffix("Desktop")
                .filter(|s| !s.is_empty())
                .unwrap_or(last_seg)
                .to_owned()
        });
    let proc_hint = format!("{last_seg}.exe");
    (app, proc_hint)
}

/// Filters out WinBridge's own tray balloon notifications to avoid self-echo.
pub fn should_skip_notification(app_name: &str, process_name: &str, title: &str) -> bool {
    let app_l = app_name.to_ascii_lowercase();
    let proc_l = process_name.to_ascii_lowercase();
    let title_l = title.to_ascii_lowercase();
    app_l.contains("win-host")
        || app_l.contains("winbridge")
        || proc_l.contains("win-host")
        || proc_l.contains("winbridge")
        || title_l.starts_with("winbridge ")
        || title_l.starts_with("winbridge配对")
}

/// Finds the best matching open top-level Windows window for a notification.
pub fn match_window_id(windows: &[WindowInfo], process_hint: &str, app_name: &str) -> u64 {
    let proc_l = process_hint.trim().to_ascii_lowercase();
    let proc_stem = proc_l.strip_suffix(".exe").unwrap_or(&proc_l);
    let app_l = app_name.trim().to_ascii_lowercase();

    // 1. Exact process_name match (excluding popups kind == 3).
    if !proc_l.is_empty() {
        if let Some(w) = windows
            .iter()
            .find(|w| w.kind != 3 && w.process_name.eq_ignore_ascii_case(&proc_l))
        {
            return w.window_id;
        }
    }
    // 2. Process stem match (e.g. `WhatsAppDesktop` matching `WhatsApp.exe`).
    if proc_stem.len() >= 3 {
        if let Some(w) = windows.iter().find(|w| {
            if w.kind == 3 {
                return false;
            }
            let w_l = w.process_name.to_ascii_lowercase();
            let w_stem = w_l.strip_suffix(".exe").unwrap_or(&w_l);
            !w_stem.is_empty() && (proc_stem.contains(w_stem) || w_stem.contains(proc_stem))
        }) {
            return w.window_id;
        }
    }
    // 3. Fallback: window title contains app_name.
    if app_l.len() >= 2 {
        if let Some(w) = windows
            .iter()
            .find(|w| w.kind != 3 && w.title.to_ascii_lowercase().contains(&app_l))
        {
            return w.window_id;
        }
    }
    0
}

// ============================================================================
// Inbox `winsqlite3.dll` Dynamic Loader (Windows Only)
// ============================================================================

#[cfg(windows)]
mod sqlite {
    use std::{
        ffi::{c_char, c_int, c_void, CString},
        mem,
        path::Path,
        ptr,
        sync::OnceLock,
    };
    use windows_sys::Win32::{
        Foundation::HMODULE,
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    };

    const SQLITE_OK: c_int = 0;
    const SQLITE_ROW: c_int = 100;
    // Open our private snapshot copy RW so SQLite can cleanly replay a copied `-wal` journal.
    const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
    const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;

    type OpenV2Fn = unsafe extern "system" fn(
        filename: *const c_char,
        pp_db: *mut *mut c_void,
        flags: c_int,
        z_vfs: *const c_char,
    ) -> c_int;
    type CloseFn = unsafe extern "system" fn(db: *mut c_void) -> c_int;
    type PrepareV2Fn = unsafe extern "system" fn(
        db: *mut c_void,
        z_sql: *const c_char,
        n_byte: c_int,
        pp_stmt: *mut *mut c_void,
        pz_tail: *mut *const c_char,
    ) -> c_int;
    type BindInt64Fn = unsafe extern "system" fn(stmt: *mut c_void, idx: c_int, val: i64) -> c_int;
    type StepFn = unsafe extern "system" fn(stmt: *mut c_void) -> c_int;
    type ColumnInt64Fn = unsafe extern "system" fn(stmt: *mut c_void, col: c_int) -> i64;
    type ColumnTextFn = unsafe extern "system" fn(stmt: *mut c_void, col: c_int) -> *const u8;
    type ColumnBlobFn = unsafe extern "system" fn(stmt: *mut c_void, col: c_int) -> *const c_void;
    type ColumnBytesFn = unsafe extern "system" fn(stmt: *mut c_void, col: c_int) -> c_int;
    type FinalizeFn = unsafe extern "system" fn(stmt: *mut c_void) -> c_int;

    struct Api {
        _module: HMODULE,
        open_v2: OpenV2Fn,
        close: CloseFn,
        prepare_v2: PrepareV2Fn,
        bind_int64: BindInt64Fn,
        step: StepFn,
        column_int64: ColumnInt64Fn,
        column_text: ColumnTextFn,
        column_blob: ColumnBlobFn,
        column_bytes: ColumnBytesFn,
        finalize: FinalizeFn,
    }

    // SAFETY: Loaded DLL handle and exported function pointers live for process lifetime and are immutable.
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    static API: OnceLock<Option<Api>> = OnceLock::new();

    macro_rules! load_fn {
        ($module:expr, $name:expr, $ty:ty) => {{
            let proc = GetProcAddress($module, $name.as_ptr())?;
            mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(proc)
        }};
    }

    fn load_api() -> Option<&'static Api> {
        API.get_or_init(|| unsafe {
            let dll: Vec<u16> = "winsqlite3.dll\0".encode_utf16().collect();
            let module = LoadLibraryW(dll.as_ptr());
            if module.is_null() {
                return None;
            }
            Some(Api {
                _module: module,
                open_v2: load_fn!(module, b"sqlite3_open_v2\0", OpenV2Fn),
                close: load_fn!(module, b"sqlite3_close\0", CloseFn),
                prepare_v2: load_fn!(module, b"sqlite3_prepare_v2\0", PrepareV2Fn),
                bind_int64: load_fn!(module, b"sqlite3_bind_int64\0", BindInt64Fn),
                step: load_fn!(module, b"sqlite3_step\0", StepFn),
                column_int64: load_fn!(module, b"sqlite3_column_int64\0", ColumnInt64Fn),
                column_text: load_fn!(module, b"sqlite3_column_text\0", ColumnTextFn),
                column_blob: load_fn!(module, b"sqlite3_column_blob\0", ColumnBlobFn),
                column_bytes: load_fn!(module, b"sqlite3_column_bytes\0", ColumnBytesFn),
                finalize: load_fn!(module, b"sqlite3_finalize\0", FinalizeFn),
            })
        })
        .as_ref()
    }

    pub fn is_available() -> bool {
        load_api().is_some()
    }

    struct DbGuard {
        api: &'static Api,
        db: *mut c_void,
    }

    impl DbGuard {
        fn open(path: &Path) -> Option<Self> {
            let api = load_api()?;
            let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
            let mut db: *mut c_void = ptr::null_mut();
            let rc = unsafe {
                (api.open_v2)(c_path.as_ptr(), &mut db, SQLITE_OPEN_READWRITE, ptr::null())
            };
            if rc != SQLITE_OK || db.is_null() {
                if !db.is_null() {
                    unsafe {
                        (api.close)(db);
                    }
                    db = ptr::null_mut();
                }
                let rc_ro = unsafe {
                    (api.open_v2)(c_path.as_ptr(), &mut db, SQLITE_OPEN_READONLY, ptr::null())
                };
                if rc_ro != SQLITE_OK || db.is_null() {
                    if !db.is_null() {
                        unsafe {
                            (api.close)(db);
                        }
                    }
                    return None;
                }
            }
            Some(Self { api, db })
        }
    }

    impl Drop for DbGuard {
        fn drop(&mut self) {
            if !self.db.is_null() {
                unsafe {
                    (self.api.close)(self.db);
                }
            }
        }
    }

    struct StmtGuard {
        api: &'static Api,
        stmt: *mut c_void,
    }

    impl Drop for StmtGuard {
        fn drop(&mut self) {
            if !self.stmt.is_null() {
                unsafe {
                    (self.api.finalize)(self.stmt);
                }
            }
        }
    }

    pub fn query_max_id(db_path: &Path) -> Option<i64> {
        let guard = DbGuard::open(db_path)?;
        let sql = b"SELECT COALESCE(MAX(Id), 0) FROM Notification\0";
        let mut stmt: *mut c_void = ptr::null_mut();
        let rc = unsafe {
            (guard.api.prepare_v2)(
                guard.db,
                sql.as_ptr() as *const c_char,
                -1,
                &mut stmt,
                ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return None;
        }
        let stmt_guard = StmtGuard {
            api: guard.api,
            stmt,
        };
        let step_rc = unsafe { (stmt_guard.api.step)(stmt_guard.stmt) };
        if step_rc == SQLITE_ROW {
            Some(unsafe { (stmt_guard.api.column_int64)(stmt_guard.stmt, 0) })
        } else {
            Some(0)
        }
    }

    pub fn query_toasts_after(db_path: &Path, last_id: i64) -> Option<Vec<(i64, String, String)>> {
        let guard = DbGuard::open(db_path)?;
        let sql = concat!(
            "SELECT n.Id, COALESCE(h.PrimaryId, ''), n.Payload ",
            "FROM Notification n ",
            "LEFT JOIN NotificationHandler h ON n.HandlerId = h.RecordId ",
            "WHERE n.Type = 'toast' AND n.Id > ?1 ",
            "ORDER BY n.Id ASC LIMIT 16\0"
        );
        let mut stmt: *mut c_void = ptr::null_mut();
        let rc = unsafe {
            (guard.api.prepare_v2)(
                guard.db,
                sql.as_ptr() as *const c_char,
                -1,
                &mut stmt,
                ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return None;
        }
        let stmt_guard = StmtGuard {
            api: guard.api,
            stmt,
        };
        if unsafe { (stmt_guard.api.bind_int64)(stmt_guard.stmt, 1, last_id) } != SQLITE_OK {
            return None;
        }
        let mut rows = Vec::new();
        while unsafe { (stmt_guard.api.step)(stmt_guard.stmt) } == SQLITE_ROW {
            let id = unsafe { (stmt_guard.api.column_int64)(stmt_guard.stmt, 0) };
            let primary_id = unsafe {
                let p = (stmt_guard.api.column_text)(stmt_guard.stmt, 1);
                let len = (stmt_guard.api.column_bytes)(stmt_guard.stmt, 1);
                if p.is_null() || len <= 0 {
                    String::new()
                } else {
                    String::from_utf8_lossy(std::slice::from_raw_parts(p, len as usize))
                        .into_owned()
                }
            };
            let payload_xml = unsafe {
                let p = (stmt_guard.api.column_blob)(stmt_guard.stmt, 2);
                let len = (stmt_guard.api.column_bytes)(stmt_guard.stmt, 2);
                if p.is_null() || len <= 0 {
                    String::new()
                } else {
                    String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, len as usize))
                        .into_owned()
                }
            };
            rows.push((id, primary_id, payload_xml));
        }
        Some(rows)
    }
}

// ============================================================================
// NotificationWatcher
// ============================================================================

pub struct NotificationWatcher {
    enabled: bool,
    src_db: PathBuf,
    src_wal: PathBuf,
    temp_dir: PathBuf,
    last_stamp: Option<DbStamp>,
    last_seen_id: i64,
    seeded: bool,
}

impl NotificationWatcher {
    pub fn new() -> Self {
        let seq = WATCHER_SEQ.fetch_add(1, Ordering::Relaxed);
        let temp_dir = std::env::temp_dir().join(format!(
            "winbridge-wpn-{}-{seq}",
            std::process::id()
        ));

        #[cfg(windows)]
        let (enabled, src_db, src_wal) = {
            let local_app_data = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_default();
            let dir = local_app_data.join("Microsoft").join("Windows").join("Notifications");
            let db = dir.join("wpndatabase.db");
            let wal = dir.join("wpndatabase.db-wal");
            let ok = !local_app_data.as_os_str().is_empty() && db.exists() && sqlite::is_available();
            if ok {
                tracing::debug!(path = %db.display(), "Windows notification watcher initialized");
            }
            (ok, db, wal)
        };

        #[cfg(not(windows))]
        let (enabled, src_db, src_wal) = (false, PathBuf::new(), PathBuf::new());

        Self {
            enabled,
            src_db,
            src_wal,
            temp_dir,
            last_stamp: None,
            last_seen_id: 0,
            seeded: false,
        }
    }

    fn current_stamp(&self) -> DbStamp {
        let (db_len, db_mtime_ns) = file_stamp(&self.src_db);
        let (wal_len, wal_mtime_ns) = file_stamp(&self.src_wal);
        DbStamp {
            db_len,
            db_mtime_ns,
            wal_len,
            wal_mtime_ns,
        }
    }

    #[cfg(windows)]
    fn copy_snapshot(&self) -> Option<PathBuf> {
        std::fs::create_dir_all(&self.temp_dir).ok()?;
        let dst_db = self.temp_dir.join("wpn.db");
        let dst_wal = self.temp_dir.join("wpn.db-wal");
        let dst_shm = self.temp_dir.join("wpn.db-shm");
        let _ = std::fs::remove_file(&dst_wal);
        let _ = std::fs::remove_file(&dst_shm);

        std::fs::copy(&self.src_db, &dst_db).ok()?;
        if self.src_wal.exists() {
            let _ = std::fs::copy(&self.src_wal, &dst_wal);
        }
        Some(dst_db)
    }

    /// Polls for any newly arrived Windows Action Center toast notifications since the previous call.
    /// The first call seeds `last_seen_id` and returns empty so historical toasts are never replayed.
    pub fn poll(&mut self) -> Vec<HostNotification> {
        if !self.enabled {
            return Vec::new();
        }
        let stamp = self.current_stamp();
        if stamp.db_len == 0 {
            return Vec::new();
        }
        if self.seeded && self.last_stamp == Some(stamp) {
            return Vec::new();
        }

        #[cfg(windows)]
        {
            let Some(snap_db) = self.copy_snapshot() else {
                return Vec::new();
            };
            if !self.seeded {
                if let Some(max_id) = sqlite::query_max_id(&snap_db) {
                    self.last_seen_id = max_id;
                    self.last_stamp = Some(stamp);
                    self.seeded = true;
                }
                return Vec::new();
            }
            self.last_stamp = Some(stamp);
            let Some(rows) = sqlite::query_toasts_after(&snap_db, self.last_seen_id) else {
                return Vec::new();
            };
            if rows.is_empty() {
                return Vec::new();
            }
            let open_windows = crate::win32::list(false).unwrap_or_default();
            let now_ms = current_unix_ms();
            let mut out = Vec::new();
            for (id, primary_id, payload_xml) in rows {
                if id > self.last_seen_id {
                    self.last_seen_id = id;
                }
                let (title, body) = parse_toast_xml(&payload_xml);
                if title.is_empty() && body.is_empty() {
                    continue;
                }
                let (app_name, process_name) = humanize_primary_id(&primary_id);
                if should_skip_notification(&app_name, &process_name, &title) {
                    continue;
                }
                let window_id = match_window_id(&open_windows, &process_name, &app_name);
                out.push(HostNotification {
                    notification_id: id.max(0) as u64,
                    app_name,
                    process_name,
                    title,
                    body,
                    window_id,
                    timestamp_ms: now_ms,
                });
            }
            out
        }

        #[cfg(not(windows))]
        {
            let _ = stamp;
            Vec::new()
        }
    }
}

impl Default for NotificationWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NotificationWatcher {
    fn drop(&mut self) {
        if self.temp_dir.exists() {
            let _ = std::fs::remove_dir_all(&self.temp_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_toast_xml_and_unescapes_entities() {
        let xml = r#"<toast><visual><binding template="ToastGeneric">
            <text id="1">Alice &amp; Bob</text>
            <text hint-maxLines="2">Are you ready for &quot;v0.5&quot;? &#x1F680;</text>
            <text>Line 3 &lt;ok&gt;</text>
        </binding></visual></toast>"#;
        let (title, body) = parse_toast_xml(xml);
        assert_eq!(title, "Alice & Bob");
        assert_eq!(body, "Are you ready for \"v0.5\"? 🚀\nLine 3 <ok>");
    }

    #[test]
    fn skips_self_closing_and_empty_text_nodes() {
        let xml = r#"<toast><visual><binding><text/><text id="1">   </text><text id="2">Build Succeeded</text></binding></visual></toast>"#;
        let (title, body) = parse_toast_xml(xml);
        assert_eq!(title, "Build Succeeded");
        assert_eq!(body, "");
    }

    #[test]
    fn humanizes_exe_paths_aumids_and_squirrel_ids() {
        assert_eq!(
            humanize_primary_id(r"{6D809377-6AF0-444B-8957-A3773F02200E}\Tencent\WeChat\WeChat.exe"),
            ("微信".to_owned(), "WeChat.exe".to_owned())
        );
        assert_eq!(
            humanize_primary_id("5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App"),
            ("WhatsApp".to_owned(), "WhatsAppDesktop.exe".to_owned())
        );
        assert_eq!(
            humanize_primary_id("com.squirrel.Discord.Discord"),
            ("Discord".to_owned(), "Discord.exe".to_owned())
        );
        assert_eq!(
            humanize_primary_id("Microsoft.Teams_8wekyb3d8bbwe!MSTeams"),
            ("Microsoft Teams".to_owned(), "Teams.exe".to_owned())
        );
    }

    #[test]
    fn matches_open_window_by_process_and_skips_winbridge_self_toasts() {
        let windows = vec![
            WindowInfo {
                window_id: 0x101,
                process_name: "WeChat.exe".into(),
                title: "微信".into(),
                kind: 1,
                ..Default::default()
            },
            WindowInfo {
                window_id: 0x202,
                process_name: "WhatsApp.exe".into(),
                title: "WhatsApp".into(),
                kind: 1,
                ..Default::default()
            },
        ];
        assert_eq!(match_window_id(&windows, "WeChat.exe", "微信"), 0x101);
        assert_eq!(match_window_id(&windows, "WhatsAppDesktop.exe", "WhatsApp"), 0x202);
        assert_eq!(match_window_id(&windows, "Unknown.exe", "Nothing"), 0);
        assert!(should_skip_notification("win-host", "win-host.exe", "WinBridge 配对码已复制"));
        assert!(!should_skip_notification("微信", "WeChat.exe", "新消息"));
    }
}
