mod media;
use crate::{
    config::{Codec, Config},
    proto::{control_message::Payload, *},
    session,
};
use anyhow::{bail, ensure, Context, Result};
pub(crate) use media::Mux;
use prost::Message;
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch, Semaphore},
    time::timeout,
};
use uuid::Uuid;

const IDLE: Duration = Duration::from_secs(15);
const MAX_CONTROL: usize = 16 << 20;
pub(crate) fn micros() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_micros() as u64
}

async fn read_message(reader: &mut (impl AsyncRead + Unpin)) -> Result<Payload> {
    let len = reader.read_u32().await? as usize;
    ensure!(
        len > 0 && len <= MAX_CONTROL,
        "invalid control frame length {len}"
    );
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    ControlMessage::decode(&bytes[..])?
        .payload
        .context("empty control message")
}

async fn write_message(
    writer: &mut (impl AsyncWrite + Unpin),
    seq: u64,
    payload: Payload,
) -> Result<()> {
    let message = ControlMessage {
        seq,
        timestamp_us: micros(),
        payload: Some(payload),
    };
    ensure!(
        message.encoded_len() <= MAX_CONTROL,
        "control payload exceeds 16 MiB"
    );
    let data = message.encode_to_vec();
    writer.write_u32(data.len() as u32).await?;
    writer.write_all(&data).await?;
    Ok(())
}

struct Active {
    id: Uuid,
    media_bound: AtomicBool,
    stop: watch::Sender<bool>,
    mux: Arc<Mux>,
}

struct Server {
    config: Arc<Config>,
    active: Mutex<Option<Arc<Active>>>,
    h264: bool,
}

fn negotiate(pref: Codec, offered: &[i32], h264: bool) -> Option<i32> {
    let supported: &[i32] = match pref {
        Codec::Auto => &[1, 3, 4],
        Codec::H264 => &[1],
        Codec::Jpeg => &[3],
        Codec::Raw => &[4],
    };
    offered
        .iter()
        .copied()
        .find(|c| supported.contains(c) && (*c != 1 || h264))
}

struct Lease<'a>(&'a Server, Arc<Active>);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.1.stop.send_replace(true);
        self.0
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

async fn control(server: &Server, mut socket: TcpStream) -> Result<()> {
    let Payload::Hello(hello) = timeout(IDLE, read_message(&mut socket)).await?? else {
        bail!("expected Hello")
    };
    let codec = negotiate(server.config.codec, &hello.supported_codecs, server.h264);
    let mut ack = HelloAck {
        host_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),
        default_fps: server.config.fps,
        ..Default::default()
    };
    let active = {
        let mut slot = server.active.lock().unwrap_or_else(|e| e.into_inner());
        if hello.protocol_version != 1 {
            ack.reject_reason = "unsupported protocol version".into();
        } else if !server.config.token.is_empty() && hello.auth_token != server.config.token {
            ack.reject_reason = "authentication failed".into();
        } else if slot.is_some() {
            ack.reject_reason = "another control session is active".into();
        } else if let Some(selected) = codec {
            let id = Uuid::new_v4();
            ack.accepted = true;
            ack.session_id = id.as_bytes().to_vec();
            ack.selected_codec = selected;
            let active = Arc::new(Active {
                id,
                media_bound: AtomicBool::new(false),
                stop: watch::channel(false).0,
                mux: Arc::new(Mux::default()),
            });
            *slot = Some(active.clone());
        } else {
            ack.reject_reason = "no common video codec".into();
        }
        if ack.accepted {
            slot.clone()
        } else {
            None
        }
    };
    let selected = ack.selected_codec;
    let lease = active.map(|active| Lease(server, active));
    timeout(IDLE, write_message(&mut socket, 1, Payload::HelloAck(ack))).await??;
    let Some(lease) = lease else { return Ok(()) };
    let (mut reader, mut writer) = socket.into_split();
    let (requests, rx) = mpsc::channel(32);
    let (replies, mut tx) = mpsc::channel(64);
    let read = async move {
        loop {
            let msg = timeout(IDLE, read_message(&mut reader)).await??;
            requests.send(msg).await.context("session ended")?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let write = async move {
        let mut seq = 2u64;
        while let Some(msg) = tx.recv().await {
            timeout(IDLE, write_message(&mut writer, seq, msg)).await??;
            seq = seq.wrapping_add(1);
        }
        Ok(())
    };
    tokio::select! {
        res = read => res,
        res = write => res,
        res = session::run(server.config.clone(), selected, lease.1.mux.clone(), rx, replies) => res,
    }
}

async fn media(server: &Server, mut socket: TcpStream) -> Result<()> {
    let mut id = [0; 16];
    timeout(IDLE, socket.read_exact(&mut id)).await??;
    let active = server
        .active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|s| {
            s.id.as_bytes() == &id
                && s.media_bound
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
        })
        .cloned();
    let Some(active) = active else {
        socket.write_u8(1).await?;
        return Ok(());
    };
    let mut cancel = active.stop.subscribe();
    let send = async {
        if *cancel.borrow() { return Ok(()) }
        active.mux.reset();
        socket.write_u8(0).await?;
        tokio::select! { result = active.mux.send(&mut socket) => result, _ = cancel.changed() => Ok(()) }
    }.await;
    active.media_bound.store(false, Ordering::Release);
    send
}

pub async fn serve(config: Config, h264: bool) -> Result<()> {
    let listener = TcpListener::bind((config.bind, config.port)).await?;
    tracing::info!("WinBridge listening on {}", listener.local_addr()?);
    let state = Arc::new(Server {
        config: Arc::new(config),
        active: Mutex::new(None),
        h264,
    });
    let connections = Arc::new(Semaphore::new(64));
    loop {
        let (mut socket, peer) = listener.accept().await?;
        socket.set_nodelay(true)?;
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let run = async {
                let mut prefix = [0; 8];
                timeout(IDLE, socket.read_exact(&mut prefix)).await??;
                match &prefix {
                    b"WBRGCTL\x01" => control(&state, socket).await,
                    b"WBRGMED\x01" => media(&state, socket).await,
                    _ => bail!("unknown connection prefix"),
                }
            }
            .await;
            if let Err(e) = run {
                if e.downcast_ref::<io::Error>()
                    .is_none_or(|v| v.kind() != io::ErrorKind::UnexpectedEof)
                {
                    tracing::debug!(%peer, error=%e, "connection closed");
                }
            }
        });
    }
}
