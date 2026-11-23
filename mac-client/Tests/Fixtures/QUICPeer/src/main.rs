//! Isolated Quinn peer for the macOS Network.framework interoperability regression.
//! Loopback only; synthetic HelloAck; no host control, clipboard or real credentials.
use std::{sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert.der().clone()], key.into())?;
    tls.alpn_protocols = vec![b"winbridge-v1".to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(128u32.into());
    transport.max_concurrent_uni_streams(128u32.into());
    config.transport_config(Arc::new(transport));
    let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse()?)?;
    println!("PORT={}", endpoint.local_addr()?.port());
    // Entire fixture is bounded even if a client fails or never arrives.
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(300)) => {},
        _ = async {
            while let Some(incoming) = endpoint.accept().await {
                tokio::spawn(async move {
                    if let Err(e) = serve(incoming).await { eprintln!("peer: {e}"); }
                });
            }
        } => {},
    }
    endpoint.close(0u32.into(), b"fixture finished");
    Ok(())
}

async fn serve(incoming: quinn::Incoming) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let conn = incoming.await?;
    // Match win-host: the first accepted bidirectional stream must be Control.
    let (mut send, mut recv) = conn.accept_bi().await?;
    println!("CONTROL_STREAM={}", recv.id());
    let mut prefix = [0; 8];
    tokio::time::timeout(Duration::from_secs(5), recv.read_exact(&mut prefix)).await??;
    assert_eq!(&prefix, b"WBRGCTL\x01");
    let mut length = [0; 4];
    recv.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    assert!(length > 0 && length < 4096);
    recv.read_exact(&mut vec![0; length]).await?;
    println!("HELLO_RECEIVED");
    // ControlMessage.hello_ack: accepted, fixed synthetic session id, host, JPEG, fps.
    let mut ack = vec![8, 1, 26, 16];
    ack.extend_from_slice(&[42; 16]);
    ack.extend_from_slice(&[34, 7]);
    ack.extend_from_slice(b"Fixture");
    ack.extend_from_slice(&[40, 3, 48, 60]);
    let mut frame = vec![90, ack.len() as u8];
    frame.extend(ack);
    send.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    send.write_all(&frame).await?;
    let (mut bulk_send, mut bulk_recv) = conn.accept_bi().await?;
    let mut bind = [0; 24];
    bulk_recv.read_exact(&mut bind).await?;
    assert_eq!(&bind[..8], b"WBRGFLS\x01");
    assert_eq!(&bind[8..], &[42; 16]);
    bulk_send.write_all(&[0]).await?;
    println!("BULK_BOUND");
    // Synthetic terminal frames test delivery/routing, not video decoding.
    for window_id in [11u64, 12] {
        let mut video = conn.open_uni().await?;
        video.write_all(b"WBRGVID\x01").await?;
        let mut frame = b"WBMF".to_vec();
        frame.extend_from_slice(&window_id.to_be_bytes());
        frame.extend_from_slice(&0u64.to_be_bytes());
        frame.extend_from_slice(&1u16.to_be_bytes());
        frame.extend_from_slice(&1u16.to_be_bytes());
        frame.extend_from_slice(&[3, 4, 0, 1]); // JPEG, end-of-stream, sequence 1
        frame.extend_from_slice(&1u32.to_be_bytes());
        frame.push(7);
        video.write_all(&frame).await?;
        video.finish()?;
    }
    let _ = conn.closed().await;
    Ok(())
}
