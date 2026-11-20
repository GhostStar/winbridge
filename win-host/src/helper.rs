use crate::{
    input::DirectExecutor,
    proto::{control_message::Payload, ControlMessage, WindowUpdated},
    win32::desktop::DesktopState,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use prost::Message;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Mutex, OnceLock},
    time::Duration,
};

const OP_PROBE: u8 = 1;
const OP_CAPTURE: u8 = 2;
const OP_INPUT: u8 = 3;

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_MSG_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
struct ClientConfig {
    addr: SocketAddr,
    token: Vec<u8>,
}

static CLIENT_CONFIG: OnceLock<ClientConfig> = OnceLock::new();
static PROBE_CONN: OnceLock<Mutex<Option<TcpStream>>> = OnceLock::new();
static CAPTURE_CONN: OnceLock<Mutex<Option<TcpStream>>> = OnceLock::new();
static INPUT_CONN: OnceLock<Mutex<Option<TcpStream>>> = OnceLock::new();

pub fn configure_client(port: u16, token: String) {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let _ = CLIENT_CONFIG.set(ClientConfig {
        addr,
        token: token.into_bytes(),
    });
}

pub fn is_configured() -> bool {
    CLIENT_CONFIG.get().is_some()
}

fn connect(cfg: &ClientConfig, timeout: Duration) -> Result<TcpStream> {
    let mut stream = TcpStream::connect_timeout(&cfg.addr, timeout)
        .with_context(|| format!("connect secure-helper on {}", cfg.addr))?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let len = u16::try_from(cfg.token.len()).context("helper token too long")?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&cfg.token)?;
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack)?;
    ensure!(ack[0] == 1, "secure-helper rejected authentication token");
    Ok(stream)
}

fn with_conn<T>(
    slot: &OnceLock<Mutex<Option<TcpStream>>>,
    timeout: Duration,
    mut f: impl FnMut(&mut TcpStream) -> Result<T>,
) -> Result<T> {
    let cfg = CLIENT_CONFIG
        .get()
        .ok_or_else(|| anyhow!("secure-helper not configured"))?;
    let mutex = slot.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
    for attempt in 0..2 {
        if guard.is_none() {
            *guard = Some(connect(cfg, timeout)?);
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

fn read_error(s: &mut TcpStream) -> Result<String> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    ensure!(len <= MAX_MSG_BYTES, "error message too large");
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn write_error(s: &mut TcpStream, msg: &str) -> Result<()> {
    let bytes = msg.as_bytes();
    let len = (bytes.len().min(MAX_MSG_BYTES)) as u32;
    s.write_all(&[0])?;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(&bytes[..len as usize])?;
    Ok(())
}

/// Runs the LocalSystem + UIAccess loopback IPC server (`--secure-helper`) inside the
/// active interactive session to handle Winlogon (UAC / Lock Screen) capture & input,
/// as well as UIPI-exempt input into elevated Administrator windows on the Default desktop.
pub fn run_server(port: u16, token: String) -> Result<()> {
    ensure!(!token.is_empty(), "--helper-token must not be empty");
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)
        .with_context(|| format!("bind secure-helper loopback listener on {addr}"))?;
    let ui_access = crate::win32::desktop::current_process_ui_access();
    tracing::info!(
        pid = std::process::id(),
        %addr,
        ui_access = ?ui_access,
        "secure-helper listening on loopback"
    );

    let expected = token.into_bytes();
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else {
            continue;
        };
        let expected_clone = expected.clone();
        let _ = std::thread::Builder::new()
            .name("secure-helper-conn".into())
            .spawn(move || {
                if let Err(e) = handle_connection(stream, &expected_clone) {
                    tracing::debug!(error = %e, "secure-helper client disconnected");
                }
            });
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, expected_token: &[u8]) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let mut len_buf = [0u8; 2];
    stream.read_exact(&mut len_buf)?;
    let tok_len = u16::from_be_bytes(len_buf) as usize;
    if tok_len != expected_token.len() || tok_len > 256 {
        let _ = stream.write_all(&[0]);
        bail!("invalid helper token length");
    }
    let mut got = vec![0u8; tok_len];
    stream.read_exact(&mut got)?;
    if got != expected_token {
        let _ = stream.write_all(&[0]);
        bail!("helper token mismatch");
    }
    stream.write_all(&[1])?;
    let _ = stream.set_read_timeout(None);

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
