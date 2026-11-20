mod common;
use common::*;
use std::io::{Read, Write};
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
