use crate::{
    input::DirectExecutor,
    proto::{control_message::Payload, ControlMessage, WindowUpdated},
    win32::desktop::DesktopState,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use prost::Message;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    mem,
    os::windows::io::{FromRawHandle, RawHandle},
    ptr,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE,
        INVALID_HANDLE_VALUE,
    },
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, PSECURITY_DESCRIPTOR,
        SECURITY_ATTRIBUTES,
    },
    Storage::FileSystem::PIPE_ACCESS_DUPLEX,
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
        WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    },
};

const OP_PROBE: u8 = 1;
const OP_CAPTURE: u8 = 2;
const OP_INPUT: u8 = 3;

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_MSG_BYTES: usize = 4 * 1024 * 1024;

static HELPER_PIPE_NAME: OnceLock<String> = OnceLock::new();
static PROBE_CONN: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static CAPTURE_CONN: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static INPUT_CONN: OnceLock<Mutex<Option<File>>> = OnceLock::new();

pub fn configure_client(pipe_name: String) {
    let _ = HELPER_PIPE_NAME.set(pipe_name);
}

pub fn is_configured() -> bool {
    HELPER_PIPE_NAME.get().is_some()
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn connect_pipe(pipe_name: &str, timeout: Duration) -> Result<File> {
    let deadline = Instant::now() + timeout;
    let wide_name = to_wide(pipe_name);
    loop {
        match OpenOptions::new().read(true).write(true).open(pipe_name) {
            Ok(file) => return Ok(file),
            Err(err) => {
                let code = err.raw_os_error().unwrap_or(0) as u32;
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(err)
                        .with_context(|| format!("connect secure-helper named pipe {pipe_name}"));
                }
                if code == ERROR_PIPE_BUSY {
                    let wait_ms = remaining.as_millis().clamp(50, 1500) as u32;
                    unsafe {
                        WaitNamedPipeW(wide_name.as_ptr(), wait_ms);
                    }
                } else {
                    std::thread::sleep(remaining.min(Duration::from_millis(40)));
                }
            }
        }
    }
}

fn with_conn<T>(
    slot: &OnceLock<Mutex<Option<File>>>,
    timeout: Duration,
    mut f: impl FnMut(&mut File) -> Result<T>,
) -> Result<T> {
    let pipe_name = HELPER_PIPE_NAME
        .get()
        .ok_or_else(|| anyhow!("secure-helper not configured"))?;
    let mutex = slot.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
    for attempt in 0..2 {
        if guard.is_none() {
            *guard = Some(connect_pipe(pipe_name, timeout)?);
        }
        let stream = guard.as_mut().unwrap();
        match f(stream) {
            Ok(val) => return Ok(val),
            Err(err) => {
                *guard = None;
                if attempt == 1 {
                    return Err(err);
                }
            }
        }
    }
    unreachable!()
}

pub fn probe_desktop_state() -> Option<DesktopState> {
    if !is_configured() {
        return None;
    }
    let res = with_conn(&PROBE_CONN, Duration::from_millis(400), |s| {
        s.write_all(&[OP_PROBE])?;
        let mut resp = [0u8; 1];
        s.read_exact(&mut resp)?;
        Ok(match resp[0] {
            1 => DesktopState::SecureAccessible,
            2 => DesktopState::SecureDenied,
            _ => DesktopState::Default,
        })
    });
    match res {
        Ok(state) => Some(state),
        Err(e) => {
            tracing::debug!(error = %e, "secure-helper probe failed");
            None
        }
    }
}

pub fn capture_secure_frame() -> Result<(u16, u16, Vec<u8>)> {
    with_conn(&CAPTURE_CONN, Duration::from_secs(2), |s| {
        s.write_all(&[OP_CAPTURE])?;
        let mut ok = [0u8; 1];
        s.read_exact(&mut ok)?;
        if ok[0] == 1 {
            let mut hdr = [0u8; 8];
            s.read_exact(&mut hdr)?;
            let w = u16::from_be_bytes([hdr[0], hdr[1]]);
            let h = u16::from_be_bytes([hdr[2], hdr[3]]);
            let len = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
            ensure!(len <= MAX_FRAME_BYTES, "secure-helper frame too large");
            let mut pixels = vec![0u8; len];
            s.read_exact(&mut pixels)?;
            Ok((w, h, pixels))
        } else {
            let err = read_error(s)?;
            bail!("{err}");
        }
    })
}

pub fn forward_input(target_window_id: u64, payload: &Payload) -> Result<Option<WindowUpdated>> {
    let body = ControlMessage {
        seq: 0,
        timestamp_us: 0,
        payload: Some(payload.clone()),
    }
    .encode_to_vec();
    let body_len = u32::try_from(body.len()).context("input message too large")?;
    with_conn(&INPUT_CONN, Duration::from_secs(2), |s| {
        s.write_all(&[OP_INPUT])?;
        s.write_all(&target_window_id.to_be_bytes())?;
        s.write_all(&body_len.to_be_bytes())?;
        s.write_all(&body)?;
        let mut ok = [0u8; 1];
        s.read_exact(&mut ok)?;
        if ok[0] == 1 {
            let mut has_upd = [0u8; 1];
            s.read_exact(&mut has_upd)?;
            if has_upd[0] == 1 {
                let mut len_buf = [0u8; 4];
                s.read_exact(&mut len_buf)?;
                let len = u32::from_be_bytes(len_buf) as usize;
                ensure!(len <= MAX_MSG_BYTES, "window update too large");
                let mut raw = vec![0u8; len];
                s.read_exact(&mut raw)?;
                let upd = WindowUpdated::decode(raw.as_slice())?;
                Ok(Some(upd))
            } else {
                Ok(None)
            }
        } else {
            let err = read_error(s)?;
            bail!("{err}");
        }
    })
}

fn read_error(s: &mut impl Read) -> Result<String> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    ensure!(len <= MAX_MSG_BYTES, "error message too large");
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn write_error(s: &mut impl Write, msg: &str) -> Result<()> {
    let bytes = msg.as_bytes();
    let len = (bytes.len().min(MAX_MSG_BYTES)) as u32;
    s.write_all(&[0])?;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(&bytes[..len as usize])?;
    Ok(())
}

fn build_pipe_sddl(allowed_user_sid: Option<&str>) -> String {
    if let Some(sid) = allowed_user_sid {
        // Basic SDDL safety: Windows SID strings contain only S, digits, and hyphens.
        if sid.starts_with("S-1-") && sid.bytes().all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
        {
            return format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{sid})");
        }
    }
    "D:P(A;;GA;;;SY)(A;;GA;;;BA)".to_owned()
}

unsafe fn create_pipe_instance(pipe_name_w: &[u16], sddl_w: &[u16]) -> Result<HANDLE> {
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    if ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl_w.as_ptr(),
        1, // SDDL_REVISION_1
        &mut sd,
        ptr::null_mut(),
    ) == 0
    {
        bail!(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW failed: {}",
            io::Error::last_os_error()
        );
    }

    let sa = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };

    let handle = CreateNamedPipeW(
        pipe_name_w.as_ptr(),
        PIPE_ACCESS_DUPLEX,
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_UNLIMITED_INSTANCES,
        65_536,
        65_536,
        0,
        &sa,
    );
    LocalFree(sd);
    if handle == INVALID_HANDLE_VALUE {
        bail!("CreateNamedPipeW failed: {}", io::Error::last_os_error());
    }
    Ok(handle)
}

/// Runs the LocalSystem + UIAccess Named Pipe IPC server (`--secure-helper`) inside the active
/// interactive session. Every incoming pipe client MUST pass both the kernel SDDL DACL and exact
/// `GetNamedPipeClientProcessId == allowed_worker_pid` verification.
pub fn run_server(
    pipe_name: String,
    allowed_worker_pid: u32,
    allowed_user_sid: Option<String>,
) -> Result<()> {
    ensure!(
        pipe_name.starts_with(r"\\.\pipe\"),
        "invalid --helper-pipe name"
    );
    ensure!(allowed_worker_pid != 0, "--allowed-worker-pid must be nonzero");
    let sddl = build_pipe_sddl(allowed_user_sid.as_deref());
    let pipe_w = to_wide(&pipe_name);
    let sddl_w = to_wide(&sddl);
    let ui_access = crate::win32::desktop::current_process_ui_access();
    tracing::info!(
        pid = std::process::id(),
        pipe = %pipe_name,
        allowed_worker_pid,
        sddl = %sddl,
        ui_access = ?ui_access,
        "secure-helper listening on ACL + PID-verified Named Pipe"
    );

    loop {
        let handle = unsafe { create_pipe_instance(&pipe_w, &sddl_w)? };
        let connected = unsafe {
            ConnectNamedPipe(handle, ptr::null_mut()) != 0
                || GetLastError() == ERROR_PIPE_CONNECTED
        };
        if !connected {
            let err = io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            tracing::warn!(error = %err, "ConnectNamedPipe failed");
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let mut client_pid: u32 = 0;
        let pid_ok = unsafe { GetNamedPipeClientProcessId(handle, &mut client_pid) != 0 };
        if !pid_ok || client_pid != allowed_worker_pid {
            tracing::warn!(
                client_pid,
                expected_pid = allowed_worker_pid,
                pid_ok,
                "rejected unauthorized secure-helper Named Pipe client"
            );
            unsafe {
                DisconnectNamedPipe(handle);
                CloseHandle(handle);
            }
            continue;
        }

        let raw_handle = handle as usize;
        let _ = std::thread::Builder::new()
            .name("secure-helper-pipe".into())
            .spawn(move || {
                let handle = raw_handle as HANDLE;
                let file = unsafe { File::from_raw_handle(handle as RawHandle) };
                if let Err(e) = handle_connection(file) {
                    tracing::debug!(error = %e, "secure-helper pipe client disconnected");
                }
            });
    }
}

fn handle_connection(mut stream: File) -> Result<()> {
    let _desk_guard = crate::win32::desktop::ThreadDesktopResetGuard::new();
    let mut executor = DirectExecutor::default();

    loop {
        let mut op = [0u8; 1];
        if stream.read_exact(&mut op).is_err() {
            return Ok(());
        }
        match op[0] {
            OP_PROBE => {
                let state = match crate::win32::desktop::probe_desktop_state_local() {
                    DesktopState::Default => 0u8,
                    DesktopState::SecureAccessible => 1u8,
                    DesktopState::SecureDenied => 2u8,
                };
                stream.write_all(&[state])?;
            }
            OP_CAPTURE => {
                let result = crate::win32::desktop::sync_thread_desktop()
                    .and_then(|_| crate::win32::desktop::capture_gdi_primary_bgra());
                match result {
                    Ok((w, h, pixels)) => {
                        let len = u32::try_from(pixels.len())?;
                        stream.write_all(&[1])?;
                        stream.write_all(&w.to_be_bytes())?;
                        stream.write_all(&h.to_be_bytes())?;
                        stream.write_all(&len.to_be_bytes())?;
                        stream.write_all(&pixels)?;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "secure-helper GDI capture failed");
                        write_error(&mut stream, &format!("{e:#}"))?;
                    }
                }
            }
            OP_INPUT => {
                let mut hdr = [0u8; 12];
                stream.read_exact(&mut hdr)?;
                let target_id = u64::from_be_bytes(hdr[0..8].try_into().unwrap());
                let body_len = u32::from_be_bytes(hdr[8..12].try_into().unwrap()) as usize;
                ensure!(body_len <= MAX_MSG_BYTES, "input payload too large");
                let mut body = vec![0u8; body_len];
                stream.read_exact(&mut body)?;
                let msg = ControlMessage::decode(body.as_slice())?;
                let Some(payload) = msg.payload else {
                    write_error(&mut stream, "empty input payload")?;
                    continue;
                };
                match executor.apply_privileged(target_id, payload) {
                    Ok(Some(upd)) => {
                        let raw = upd.encode_to_vec();
                        let len = u32::try_from(raw.len())?;
                        stream.write_all(&[1, 1])?;
                        stream.write_all(&len.to_be_bytes())?;
                        stream.write_all(&raw)?;
                    }
                    Ok(None) => {
                        stream.write_all(&[1, 0])?;
                    }
                    Err(e) => {
                        tracing::warn!(target_id, error = %e, "secure-helper input failed");
                        write_error(&mut stream, &format!("{e:#}"))?;
                    }
                }
            }
            other => {
                bail!("unknown helper opcode {other}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build_pipe_sddl;

    #[test]
    fn builds_strict_pipe_sddl_with_valid_sid_only() {
        assert_eq!(
            build_pipe_sddl(Some("S-1-5-21-12345-67890-1001")),
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;S-1-5-21-12345-67890-1001)"
        );
        // Reject malformed SID injection attempts
        assert_eq!(
            build_pipe_sddl(Some("S-1-5-21-1)(A;;GA;;;WD")),
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)"
        );
        assert_eq!(build_pipe_sddl(None), "D:P(A;;GA;;;SY)(A;;GA;;;BA)");
    }
}
