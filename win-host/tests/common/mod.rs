#![allow(dead_code)]
use prost::Message;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use win_host::proto::{control_message::Payload, *};

pub const TOKEN: &str = "winbridge-loopback-test";
pub struct Host {
    child: Child,
    pub addr: SocketAddr,
}
impl Host {
    pub fn start(codec: &str) -> Self {
        Self::start_with_args(codec, &["--no-tls"])
    }
    pub fn start_with_args(codec: &str, extra: &[&str]) -> Self {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = socket.local_addr().unwrap();
        drop(socket);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_win-host"));
        cmd.args([
            "--bind",
            "127.0.0.1",
            "--port",
            &addr.port().to_string(),
            "--token",
            TOKEN,
            "--codec",
            codec,
        ])
        .args(extra);
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut host = Self { addr, child };
        let start = Instant::now();
        while TcpStream::connect(addr).is_err() {
            assert!(
                host.child.try_wait().unwrap().is_none(),
                "host exited during startup"
            );
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "startup timed out"
            );
            thread::sleep(Duration::from_millis(20));
        }
        host
    }
    pub fn connect(&self) -> TcpStream {
        let s = TcpStream::connect(self.addr).unwrap();
        s.set_nodelay(true).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
        s.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
        s
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub fn send(s: &mut TcpStream, p: Payload) {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let bytes = ControlMessage {
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
        timestamp_us: 0,
        payload: Some(p),
    }
    .encode_to_vec();
    s.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
    s.write_all(&bytes).unwrap();
}
pub fn recv(s: &mut TcpStream) -> Payload {
    let mut len = [0; 4];
    s.read_exact(&mut len).unwrap();
    let len = u32::from_be_bytes(len) as usize;
    assert!((1..=16 << 20).contains(&len));
    let mut data = vec![0; len];
    s.read_exact(&mut data).unwrap();
    ControlMessage::decode(data.as_slice())
        .unwrap()
        .payload
        .unwrap()
}
pub fn hello(s: &mut TcpStream, token: &str, codecs: Vec<i32>) -> HelloAck {
    s.write_all(b"WBRGCTL\x01").unwrap();
    send(
        s,
        Payload::Hello(Hello {
            protocol_version: 1,
            auth_token: token.into(),
            supported_codecs: codecs,
            ..Default::default()
        }),
    );
    let Payload::HelloAck(ack) = recv(s) else {
        panic!("expected HelloAck")
    };
    ack
}
pub fn media(h: &Host, id: &[u8]) -> (TcpStream, u8) {
    let mut s = h.connect();
    s.write_all(b"WBRGMED\x01").unwrap();
    s.write_all(id).unwrap();
    let mut status = [0];
    s.read_exact(&mut status).unwrap();
    (s, status[0])
}
pub struct Frame {
    pub id: u64,
    pub w: u16,
    pub h: u16,
    pub flags: u8,
    pub codec: u8,
    pub bytes: Vec<u8>,
}
pub fn frame(s: &mut TcpStream) -> Frame {
    let mut h = [0; 32];
    s.read_exact(&mut h).unwrap();
    assert_eq!(&h[..4], b"WBMF");
    let len = u32::from_be_bytes(h[28..].try_into().unwrap()) as usize;
    assert!(len <= 64 << 20);
    let mut bytes = vec![0; len];
    s.read_exact(&mut bytes).unwrap();
    Frame {
        id: u64::from_be_bytes(h[4..12].try_into().unwrap()),
        w: u16::from_be_bytes(h[20..22].try_into().unwrap()),
        h: u16::from_be_bytes(h[22..24].try_into().unwrap()),
        codec: h[24],
        flags: h[25],
        bytes,
    }
}
