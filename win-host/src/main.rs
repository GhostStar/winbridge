use anyhow::{Context, Result};
use clap::Parser;
use std::time::Duration;
use win_host::{capture, config::Config, input, net::tls, security, service, win32};

fn load_cli_token(config: &mut Config) -> Result<()> {
    if !config.token.is_empty() {
        config.token = config.token.trim().to_owned();
        return Ok(());
    }
    if let Some(path) = &config.token_file {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read --token-file {}", path.display()))?;
        config.token = raw.trim().to_owned();
        return Ok(());
    }
    if let Ok(env_tok) = std::env::var("WINBRIDGE_TOKEN") {
        let trimmed = env_tok.trim();
        if !trimmed.is_empty() {
            config.token = trimmed.to_owned();
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    win32::init_dpi();
    let mut config = Config::parse();
    let filter = tracing_subscriber::EnvFilter::try_new(&config.log_level)?;
    let log_file = if config.service || config.session_worker || config.secure_helper {
        let dir = security::program_data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let name = if config.service {
            "service.log"
        } else if config.secure_helper {
            "secure-helper.log"
        } else {
            "user-worker.log"
        };
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
            .or_else(|_| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(std::env::temp_dir().join(format!("winbridge-{name}")))
            })
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

    load_cli_token(&mut config)?;
    if let Some((width, height)) = config.virtual_display {
        match win32::apply_virtual_mode(width, height, 60) {
            Ok(message) => tracing::info!("{message}"),
            Err(e) => tracing::error!("{e:#}"),
        }
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
    if config.secure_helper {
        let pipe = config
            .helper_pipe
            .ok_or_else(|| anyhow::anyhow!("--secure-helper requires --helper-pipe"))?;
        let pid = config
            .allowed_worker_pid
            .ok_or_else(|| anyhow::anyhow!("--secure-helper requires --allowed-worker-pid"))?;
        return win_host::helper::run_server(pipe, pid, config.allowed_user_sid);
    }

    let mut stdin_tls: Option<tls::TlsMaterial> = None;
    if config.secrets_from_stdin {
        let mut stdin = std::io::stdin().lock();
        let secrets = security::WorkerSecrets::decode_from_reader(&mut stdin)
            .context("read worker secrets from service stdin pipe")?;
        if !secrets.auth_token.is_empty() {
            config.token = secrets.auth_token;
        }
        if !config.no_tls && !secrets.tls_cert_pem.is_empty() && !secrets.tls_key_pem.is_empty() {
            stdin_tls = Some(tls::from_pem_pair(
                secrets.tls_cert_pem,
                secrets.tls_key_pem,
            )?);
        }
    }

    if let Some(pipe) = &config.helper_pipe {
        win_host::helper::configure_client(pipe.clone());
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

    security::validate_auth_policy(&config)?;

    let tls_material = if config.no_tls {
        None
    } else if let Some(mat) = stdin_tls {
        Some(mat)
    } else {
        let dir = config
            .tls_dir
            .clone()
            .unwrap_or_else(|| security::user_data_dir().join("tls"));
        Some(tls::load_or_create(&dir, false)?)
    };

    let h264 = matches!(
        config.codec,
        win_host::config::Codec::Auto | win_host::config::Codec::H264
    ) && capture::supports_h264();
    let hevc = matches!(
        config.codec,
        win_host::config::Codec::Auto | win_host::config::Codec::Hevc
    ) && capture::supports_hevc();
    let caps = win_host::net::VideoCaps { h264, hevc };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(win_host::net::serve(config, caps, tls_material))
}
