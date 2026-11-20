use anyhow::Result;
use clap::Parser;
use std::time::Duration;
use win_host::{capture, config::Config, input, service, win32};

fn main() -> Result<()> {
    win32::init_dpi();
    let config = Config::parse();
    let filter = tracing_subscriber::EnvFilter::try_new(&config.log_level)?;
    let log_file = if config.service || config.session_worker {
        let dir = std::env::var_os("ProgramData")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("C:\\ProgramData"))
            .join("WinBridge");
        let _ = std::fs::create_dir_all(&dir);
        let name = if config.service {
            "service.log"
        } else {
            "worker.log"
        };
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
            .ok()
    } else {
        None
    };
    if let Some(file) = log_file {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }
    if config.install_service {
        return service::install(&config);
    }
    if config.uninstall_service {
        return service::uninstall();
    }
    if config.service {
        return service::run_service_dispatcher(config);
    }
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
            config.capture_mode == win_host::config::CaptureMode::MonitorCrop,
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
