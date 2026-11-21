mod common;
use common::*;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    DigitallySignedStruct, SignatureScheme,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use std::{
    io::{Read, Write},
    sync::Arc,
};
use win_host::proto::{control_message::Payload, *};

#[test]
fn authenticated_session_owns_exactly_one_media_connection() {
    let host = Host::start("jpeg");
    assert!(!hello(&mut host.connect(), "wrong", vec![3]).accepted);
    assert!(!hello(&mut host.connect(), TOKEN, vec![2]).accepted);
    let mut ctl = host.connect();
    let ack = hello(&mut ctl, TOKEN, vec![1, 3, 4]);
    assert!(ack.accepted);
    assert_eq!(ack.selected_codec, 3);
    assert_eq!(ack.session_id.len(), 16);
    assert!(!hello(&mut host.connect(), TOKEN, vec![3]).accepted);
    assert_eq!(media(&host, &[0; 16]).1, 1);
    let (mut stream, ok) = media(&host, &ack.session_id);
    assert_eq!(ok, 0);
    assert_eq!(media(&host, &ack.session_id).1, 1);
    send(
        &mut ctl,
        Payload::ListWindowsReq(ListWindowsRequest {
            include_icons: false,
        }),
    );
    loop {
        if matches!(recv(&mut ctl), Payload::ListWindowsRes(_)) {
            break;
        }
    }
    send(
        &mut ctl,
        Payload::Ping(Ping {
            echo_timestamp_us: 8765,
        }),
    );
    loop {
        if let Payload::Pong(p) = recv(&mut ctl) {
            assert_eq!(p.echo_timestamp_us, 8765);
            break;
        }
    }
    drop(ctl);
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(media(&host, &ack.session_id).1, 1);
}

#[test]
fn malformed_preface_and_oversized_control_length_close_the_socket() {
    let host = Host::start("raw");
    for prefix in [b"WBRGCTL\x01", b"WBRGBAD\x01"] {
        let mut s = host.connect();
        let mut bytes = prefix.to_vec();
        if prefix == b"WBRGCTL\x01" {
            bytes.extend_from_slice(&((16u32 << 20) + 1).to_be_bytes());
        }
        s.write_all(&bytes).unwrap();
        let result = s.read(&mut [0; 1]);
        assert!(
            matches!(result, Ok(0))
                || result.is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset)
        );
    }
}

#[derive(Debug)]
struct AcceptAnySelfSigned;
impl ServerCertVerifier for AcceptAnySelfSigned {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[test]
fn tls_13_encrypts_control_and_media_and_rate_limits_auth_failures() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls_dir = std::env::temp_dir().join(format!("winbridge-tls-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tls_dir);
    let tls_dir_str = tls_dir.to_string_lossy().into_owned();
    let host = Host::start_with_args("jpeg", &["--tls-dir", &tls_dir_str]);

    let client_cfg = Arc::new(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnySelfSigned))
            .with_no_client_auth(),
    );

    let connect_tls = || {
        let tcp = host.connect();
        let conn = rustls::ClientConnection::new(
            client_cfg.clone(),
            ServerName::try_from("localhost").unwrap(),
        )
        .unwrap();
        rustls::StreamOwned::new(conn, tcp)
    };

    // 1. Valid TLS Control + Media + Ping/Pong round-trip
    let mut ctl = connect_tls();
    ctl.write_all(b"WBRGCTL\x01").unwrap();
    let hello_bytes = ControlMessage {
        seq: 1,
        timestamp_us: 0,
        payload: Some(Payload::Hello(Hello {
            protocol_version: 1,
            auth_token: TOKEN.into(),
            supported_codecs: vec![3],
            ..Default::default()
        })),
    }
    .encode_to_vec();
    ctl.write_all(&(hello_bytes.len() as u32).to_be_bytes())
        .unwrap();
    ctl.write_all(&hello_bytes).unwrap();
    ctl.flush().unwrap();

    let mut len_buf = [0u8; 4];
    ctl.read_exact(&mut len_buf).unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(len_buf) as usize];
    ctl.read_exact(&mut body).unwrap();
    let Payload::HelloAck(ack) = ControlMessage::decode(body.as_slice())
        .unwrap()
        .payload
        .unwrap()
    else {
        panic!("expected HelloAck over TLS");
    };
    assert!(ack.accepted);
    assert_eq!(ack.session_id.len(), 16);

    let mut med = connect_tls();
    med.write_all(b"WBRGMED\x01").unwrap();
    med.write_all(&ack.session_id).unwrap();
    med.flush().unwrap();
    let mut status = [0u8; 1];
    med.read_exact(&mut status).unwrap();
    assert_eq!(status[0], 0);
    drop(med);
    drop(ctl);

    // 2. Three failed auth attempts trigger per-IP lockout
    for attempt in 0..4 {
        let mut s = connect_tls();
        s.write_all(b"WBRGCTL\x01").unwrap();
        let bad = ControlMessage {
            seq: 1,
            timestamp_us: 0,
            payload: Some(Payload::Hello(Hello {
                protocol_version: 1,
                auth_token: "wrong-token".into(),
                supported_codecs: vec![3],
                ..Default::default()
            })),
        }
        .encode_to_vec();
        s.write_all(&(bad.len() as u32).to_be_bytes()).unwrap();
        s.write_all(&bad).unwrap();
        s.flush().unwrap();

        let mut lb = [0u8; 4];
        s.read_exact(&mut lb).unwrap();
        let mut raw = vec![0u8; u32::from_be_bytes(lb) as usize];
        s.read_exact(&mut raw).unwrap();
        let Payload::HelloAck(ack) = ControlMessage::decode(raw.as_slice())
            .unwrap()
            .payload
            .unwrap()
        else {
            panic!("expected HelloAck");
        };
        assert!(!ack.accepted);
        if attempt == 3 {
            assert!(
                ack.reject_reason.contains("too many failed"),
                "expected rate limit lockout, got: {}",
                ack.reject_reason
            );
        }
    }

    let _ = std::fs::remove_dir_all(&tls_dir);
}
