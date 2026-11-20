use anyhow::Result;
use clap::Parser;
use std::time::Duration;
use win_host::{capture, config::Config, input, win32};

fn main() -> Result<()> {
    win32::init_dpi();
    let config = Config::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&config.log_level)?)
        .with_writer(std::io::stderr)
        .init();
    if config.list_windows {
        for w in win32::list(false)? {
            println!(
                "0x{:X}\t{}\t{}\t{}x{}\t{} dpi\t{}",
                w.window_id, w.pid, w.process_name, w.width, w.height, w.dpi, w.title
            );
        }
        return Ok(());
    }
    if let Some(id) = config.test_capture {
        std::thread::sleep(Duration::from_secs(config.delay_secs));
        return capture::probe(
            id,
            config.frames,
            config.save.as_deref(),
            config.capture_cursor,
        );
    }
    if let Some(id) = config.test_input {
        return input::probe(id);
    }
    let h264 = matches!(
        config.codec,
        win_host::config::Codec::Auto | win_host::config::Codec::H264
    ) && capture::supports_h264();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(win_host::net::serve(config, h264))
}
