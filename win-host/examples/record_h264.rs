use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use prost::Message;
use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    time::{Duration, Instant},
};
use win_host::proto::{control_message::Payload, *};

#[derive(Parser)]
#[command(about = "Record a subscribed window's H.264 Annex-B stream for playback")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9876")]
    address: SocketAddr,
    #[arg(long, default_value = "")]
    token: String,
    #[arg(long)]
    hwnd: String,
    #[arg(long, default_value_t=300, value_parser=clap::value_parser!(u32).range(1..))]
    frames: u32,
    #[arg(long, default_value = "window.h264")]
    save: PathBuf,
}
fn send(socket: &mut TcpStream, seq: u64, msg: Payload, clock: Instant) -> Result<()> {
    let data = ControlMessage {
        seq,
        timestamp_us: clock.elapsed().as_micros() as u64,
        payload: Some(msg),
    }
    .encode_to_vec();
    socket.write_all(&(data.len() as u32).to_be_bytes())?;
    socket.write_all(&data)?;
    Ok(())
}
fn receive(socket: &mut TcpStream) -> Result<Payload> {
    let mut prefix = [0; 4];
    socket.read_exact(&mut prefix)?;
    let len = u32::from_be_bytes(prefix) as usize;
    ensure!(len <= 16 << 20, "invalid control message size");
    let mut data = vec![0; len];
    socket.read_exact(&mut data)?;
    ControlMessage::decode(data.as_slice())?
        .payload
        .context("empty control message")
}
fn connect(address: SocketAddr) -> Result<TcpStream> {
    let s = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(15)))?;
    Ok(s)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let id = match args
        .hwnd
        .strip_prefix("0x")
        .or_else(|| args.hwnd.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16)?,
        None => args.hwnd.parse()?,
    };
    let clock = Instant::now();
    let mut ctl = connect(args.address)?;
    ctl.write_all(b"WBRGCTL\x01")?;
    send(
        &mut ctl,
        1,
        Payload::Hello(Hello {
            protocol_version: 1,
            client_name: "record_h264".into(),
            auth_token: args.token,
            supported_codecs: vec![1],
            ..Default::default()
        }),
        clock,
    )?;
    let Payload::HelloAck(ack) = receive(&mut ctl)? else {
        bail!("expected HelloAck")
    };
    ensure!(ack.accepted, "handshake rejected: {}", ack.reject_reason);
    ensure!(
        ack.session_id.len() == 16 && ack.selected_codec == 1,
        "invalid session or codec"
    );
    let mut media = connect(args.address)?;
    media.write_all(b"WBRGMED\x01")?;
    media.write_all(&ack.session_id)?;
    let mut status = [0];
    media.read_exact(&mut status)?;
    ensure!(status == [0], "media channel rejected");
    send(
        &mut ctl,
        2,
        Payload::SubscribeWindow(SubscribeWindow {
            window_id: id,
            ..Default::default()
        }),
        clock,
    )?;
    let mut replies = ctl.try_clone()?;
    std::thread::spawn(move || {
        while let Ok(msg) = receive(&mut replies) {
            if let Payload::HostError(e) = msg {
                eprintln!("host error {}: {}", e.code, e.message);
            }
        }
    });
    let mut writer = BufWriter::new(File::create(args.save)?);
    let mut size = (0, 0);
    let mut last_ping = Instant::now();
    let mut first = None;
    let mut seq = 3;
    let mut keys = 0;
    for _ in 0..args.frames {
        let mut h = [0; 32];
        media.read_exact(&mut h)?;
        ensure!(
            &h[..4] == b"WBMF" && h[24] == 1 && u64::from_be_bytes(h[4..12].try_into()?) == id,
            "unexpected media frame"
        );
        ensure!(h[25] & 4 == 0, "stream ended");
        let len = u32::from_be_bytes(h[28..32].try_into()?) as usize;
        ensure!(len > 0 && len <= 64 << 20, "invalid H.264 frame size");
        let mut bytes = vec![0; len];
        media.read_exact(&mut bytes)?;
        if first.is_none() {
            ensure!(h[25] & 1 != 0, "stream did not start with a keyframe");
            first = Some(Instant::now());
        }
        if h[25] & 1 != 0 {
            keys += 1;
        }
        size = (
            u16::from_be_bytes(h[20..22].try_into()?),
            u16::from_be_bytes(h[22..24].try_into()?),
        );
        writer.write_all(&bytes)?;
        if last_ping.elapsed() >= Duration::from_secs(5) {
            send(
                &mut ctl,
                seq,
                Payload::Ping(Ping {
                    echo_timestamp_us: clock.elapsed().as_micros() as u64,
                }),
                clock,
            )?;
            seq += 1;
            last_ping = Instant::now();
        }
    }
    writer.flush()?;
    let fps = f64::from(args.frames - 1) / first.unwrap().elapsed().as_secs_f64();
    println!(
        "{} frames at {}x{}, {:.1} fps, {} keyframes",
        args.frames, size.0, size.1, fps, keys
    );
    Ok(())
}
