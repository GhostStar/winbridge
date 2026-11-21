use crate::{
    net::{
        handshake_control, media::Frame, read_message, tls, write_message, Lease, Mux, Server,
        IDLE,
    },
    proto::control_message::Payload,
    security, session,
};
use anyhow::{bail, Context, Result};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
};
use tokio::{
    sync::{mpsc, watch, Mutex as AsyncMutex, Semaphore},
    time::timeout,
};

pub const PREFACE_CONTROL: &[u8; 8] = b"WBRGCTL\x01";
pub const PREFACE_BULK: &[u8; 8] = b"WBRGFLS\x01";
pub const PREFACE_AUDIO: &[u8; 8] = b"WBRGAUD\x01";
pub const PREFACE_VIDEO: &[u8; 8] = b"WBRGVID\x01";

async fn pump_quic_media(
    conn: quinn::Connection,
    mux: Arc<Mux>,
    cancel: watch::Receiver<bool>,
) -> Result<()> {
    mux.reset();

    // Dedicated high-priority unidirectional QUIC stream for low-latency audio packets.
    let audio_conn = conn.clone();
    let audio_mux = mux.clone();
    let mut audio_cancel = cancel.clone();
    let audio_task = async move {
        let mut stream: Option<quinn::SendStream> = None;
        loop {
            while let Some(frame) = audio_mux.pop_audio() {
                if stream.is_none() {
                    let mut s = audio_conn.open_uni().await?;
                    let _ = s.set_priority(10);
                    s.write_all(PREFACE_AUDIO).await?;
                    stream = Some(s);
                }
                if let Some(s) = stream.as_mut() {
                    s.write_all(&frame.header()).await?;
                    s.write_all(&frame.bytes).await?;
                }
            }
            if *audio_cancel.borrow() {
                break;
            }
            tokio::select! {
                _ = audio_mux.audio_notified() => {},
                _ = audio_cancel.changed() => break,
            }
        }
        if let Some(mut s) = stream {
            let _ = s.finish();
        }
        Ok::<(), anyhow::Error>(())
    };

    // Per-window unidirectional QUIC streams eliminate inter-window head-of-line blocking.
    let video_conn = conn;
    let video_mux = mux;
    let mut video_cancel = cancel;
    let video_task = async move {
        let mut writers: HashMap<u64, mpsc::Sender<Frame>> = HashMap::new();
        loop {
            while let Some((window_id, is_popup, frame)) = video_mux.pop_video() {
                let is_eos = frame.is_eos();
                if is_eos {
                    if let Some(tx) = writers.remove(&window_id) {
                        let _ = tx.try_send(frame);
                    }
                    continue;
                }

                let needs_spawn = writers
                    .get(&window_id)
                    .is_none_or(|tx| tx.is_closed());
                if needs_spawn {
                    let (tx, mut rx) = mpsc::channel::<Frame>(4);
                    let conn_w = video_conn.clone();
                    let mut cancel_w = video_cancel.clone();
                    tokio::spawn(async move {
                        let Ok(mut s) = conn_w.open_uni().await else {
                            return;
                        };
                        let _ = s.set_priority(if is_popup { 5 } else { 0 });
                        if s.write_all(PREFACE_VIDEO).await.is_err() {
                            return;
                        }
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancel_w.changed() => break,
                                maybe_frame = rx.recv() => match maybe_frame {
                                    Some(f) => {
                                        let eos = f.is_eos();
                                        if s.write_all(&f.header()).await.is_err() {
                                            break;
                                        }
                                        if !f.bytes.is_empty() && s.write_all(&f.bytes).await.is_err() {
                                            break;
                                        }
                                        if eos {
                                            break;
                                        }
                                    }
                                    None => break,
                                }
                            }
                        }
                        let _ = s.finish();
                    });
                    writers.insert(window_id, tx);
                }

                if let Some(tx) = writers.get(&window_id) {
                    match tx.try_send(frame) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            // Never block other windows when one stream experiences packet loss.
                            video_mux.request_keyframe(window_id);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            writers.remove(&window_id);
                        }
                    }
                }
            }
            if *video_cancel.borrow() {
                break;
            }
            tokio::select! {
                _ = video_mux.video_notified() => {},
                _ = video_cancel.changed() => break,
            }
        }
        writers.clear();
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        res = audio_task => res,
        res = video_task => res,
    }
}

async fn run_bulk_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    requests: mpsc::Sender<Payload>,
    bulk_rx: Arc<AsyncMutex<mpsc::Receiver<Payload>>>,
) -> Result<()> {
    let _ = send.set_priority(-10);
    let read_side = async move {
        loop {
            match read_message(&mut recv).await {
                Ok(msg) => {
                    requests.send(msg).await.context("session ended")?;
                }
                Err(_) => break,
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    let write_side = async move {
        let mut rx = bulk_rx.lock().await;
        let mut seq = 0x4000_0000_u64;
        while let Some(msg) = rx.recv().await {
            write_message(&mut send, seq, msg).await?;
            seq = seq.wrapping_add(1);
        }
        let _ = send.finish();
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {
        res = read_side => res,
        res = write_side => res,
    }
}

async fn handle_quic_connection(
    server: Arc<Server>,
    local_ip: Option<std::net::IpAddr>,
    conn: quinn::Connection,
) -> Result<()> {
    let peer_ip = conn.remote_address().ip();
    let (mut ctl_send, mut ctl_recv) = timeout(IDLE, conn.accept_bi())
        .await
        .context("timed out waiting for QUIC control stream")??;
    let _ = ctl_send.set_priority(15);

    let mut prefix = [0u8; 8];
    timeout(IDLE, ctl_recv.read_exact(&mut prefix)).await??;
    if &prefix != PREFACE_CONTROL {
        bail!("expected QUIC control stream prefix");
    }

    let Payload::Hello(hello) = timeout(IDLE, read_message(&mut ctl_recv)).await?? else {
        bail!("expected Hello on QUIC control stream");
    };
    let (ack, active) = handshake_control(&server, peer_ip, local_ip, hello, true).await;
    let selected = ack.selected_codec;
    let lease = active.map(|a| Lease::new(&server, a));
    timeout(IDLE, write_message(&mut ctl_send, 1, Payload::HelloAck(ack))).await??;
    let Some(lease) = lease else {
        let _ = ctl_send.finish();
        return Ok(());
    };

    tracing::info!(%peer_ip, "QUIC multi-stream session established");

    let (requests, rx) = mpsc::channel(32);
    let (replies, mut tx) = mpsc::channel(64);
    let (bulk_replies, bulk_rx_raw) = mpsc::channel(16);
    let bulk_rx = Arc::new(AsyncMutex::new(bulk_rx_raw));

    let req_ctl = requests.clone();
    let req_bulk = requests;
    let ctl_read = async move {
        loop {
            let msg = timeout(IDLE, read_message(&mut ctl_recv)).await??;
            req_ctl.send(msg).await.context("session ended")?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };

    let ctl_write = async move {
        let mut seq = 2u64;
        while let Some(msg) = tx.recv().await {
            timeout(IDLE, write_message(&mut ctl_send, seq, msg)).await??;
            seq = seq.wrapping_add(1);
        }
        let _ = ctl_send.finish();
        Ok::<(), anyhow::Error>(())
    };

    let session_uuid = lease.active().id;
    let conn_bulk = conn.clone();
    let bulk_rx_loop = bulk_rx.clone();
    let bulk_accept = async move {
        loop {
            let Ok((mut b_send, mut b_recv)) = conn_bulk.accept_bi().await else {
                break;
            };
            let mut pref = [0u8; 8];
            if !matches!(timeout(IDLE, b_recv.read_exact(&mut pref)).await, Ok(Ok(()))) {
                continue;
            }
            if &pref != PREFACE_BULK {
                let _ = b_send.write_all(&[1]).await;
                let _ = b_send.finish();
                continue;
            }
            let mut id = [0u8; 16];
            if !matches!(timeout(IDLE, b_recv.read_exact(&mut id)).await, Ok(Ok(()))) {
                continue;
            }
            if !security::constant_time_eq(session_uuid.as_bytes(), &id) {
                let _ = b_send.write_all(&[1]).await;
                let _ = b_send.finish();
                continue;
            }
            if b_send.write_all(&[0]).await.is_err() {
                continue;
            }
            tracing::debug!(%peer_ip, "QUIC isolated bulk file stream bound");
            let req = req_bulk.clone();
            let brx = bulk_rx_loop.clone();
            tokio::spawn(async move {
                let _ = run_bulk_stream(b_send, b_recv, req, brx).await;
            });
        }
        Ok::<(), anyhow::Error>(())
    };

    let media_cancel = lease.active().stop.subscribe();
    let media_mux = lease.active().mux.clone();
    let media_pump = pump_quic_media(conn.clone(), media_mux, media_cancel);

    let session_task = session::run(
        server.config.clone(),
        selected,
        lease.active().mux.clone(),
        rx,
        replies,
        bulk_replies,
    );

    let result = tokio::select! {
        res = ctl_read => res,
        res = ctl_write => res,
        res = session_task => res,
        res = media_pump => res,
        _ = bulk_accept => Ok(()),
    };
    conn.close(0u32.into(), b"session ended");
    result
}

pub(crate) async fn serve_quic(
    server: Arc<Server>,
    tls_material: tls::TlsMaterial,
    bind_addr: SocketAddr,
) -> Result<()> {
    let quic_cfg = tls_material.build_quic_server_config()?;
    let endpoint = quinn::Endpoint::server(quic_cfg, bind_addr)
        .with_context(|| format!("bind QUIC UDP socket on {bind_addr}"))?;
    let local_addr = endpoint.local_addr()?;
    tracing::info!(
        %local_addr,
        alpn = "winbridge-v1",
        "WinBridge QUIC (UDP) multi-stream listener enabled"
    );

    let connections = Arc::new(Semaphore::new(64));
    while let Some(incoming) = endpoint.accept().await {
        let peer = incoming.remote_address();
        if !server.allowlist.allows(peer.ip()) {
            tracing::warn!(%peer, "rejected QUIC connection from IP outside --allow-cidr");
            incoming.refuse();
            continue;
        }
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let local_ip = incoming.local_ip();
        let state = server.clone();
        tokio::spawn(async move {
            let _permit = permit;
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle_quic_connection(state, local_ip, conn).await {
                        tracing::debug!(%peer, error = %e, "QUIC connection closed");
                    }
                }
                Err(e) => {
                    tracing::debug!(%peer, error = %e, "QUIC handshake failed");
                }
            }
        });
    }
    Ok(())
}
