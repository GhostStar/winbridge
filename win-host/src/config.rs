use clap::{ArgGroup, Parser, ValueEnum};
use std::{net::IpAddr, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Codec {
    Auto,
    H264,
    Jpeg,
    Raw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CaptureMode {
    Window,
    MonitorCrop,
}

fn hwnd(arg: &str) -> Result<u64, String> {
    let value = match arg.strip_prefix("0x").or_else(|| arg.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => arg.parse(),
    };
    value
        .ok()
        .filter(|&v| v != 0 && v <= isize::MAX as u64)
        .ok_or_else(|| "expected a nonzero HWND in decimal or hexadecimal".to_owned())
}

#[derive(Parser)]
#[command(
    version,
    about = "Stream selected Windows application windows to a WinBridge client"
)]
#[command(group(ArgGroup::new("diagnostic").args([
    "list_windows",
    "test_capture",
    "test_input",
    "install_service",
    "uninstall_service",
    "service",
    "session_worker",
    "secure_helper",
])))]
pub struct Config {
    #[arg(long, default_value = "0.0.0.0")]
    pub bind: IpAddr,
    #[arg(long, default_value_t = 9876)]
    pub port: u16,
    #[arg(long, default_value = "")]
    pub token: String,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
    pub fps: u32,
    #[arg(long, default_value_t = 15000, value_parser = clap::value_parser!(u32).range(1..=4294967))]
    pub bitrate: u32,
    #[arg(long, value_enum, default_value = "auto")]
    pub codec: Codec,
    #[arg(long, value_enum, default_value = "window")]
    pub capture_mode: CaptureMode,
    #[arg(long)]
    pub capture_cursor: bool,
    #[arg(long, default_value = "info")]
    pub log_level: String,
    #[arg(long)]
    pub list_windows: bool,
    #[arg(long, value_parser = hwnd)]
    pub test_capture: Option<u64>,
    #[arg(long, value_parser = hwnd)]
    pub test_input: Option<u64>,
    #[arg(long, default_value_t = 0)]
    pub delay_secs: u64,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
    pub frames: u32,
    #[arg(long, requires = "test_capture")]
    pub save: Option<PathBuf>,
    /// Install and start WinBridge as a LocalSystem Windows Service (enables clicking UAC & Lock Screen).
    #[arg(long)]
    pub install_service: bool,
    /// Stop and uninstall the WinBridge Windows Service.
    #[arg(long)]
    pub uninstall_service: bool,
    /// Internal: run as Windows Service Control Manager entrypoint.
    #[arg(long, hide = true)]
    pub service: bool,
    /// Internal: run as interactive session worker spawned by the WinBridge service.
    #[arg(long, hide = true)]
    pub session_worker: bool,
    /// Internal: run as LocalSystem + UIAccess secure-desktop helper spawned by the service.
    #[arg(long, hide = true)]
    pub secure_helper: bool,
    /// Internal: loopback TCP port for session-worker <-> secure-helper IPC.
    #[arg(long, hide = true)]
    pub helper_port: Option<u16>,
    /// Internal: per-launch authentication token for session-worker <-> secure-helper IPC.
    #[arg(long, hide = true, default_value = "")]
    pub helper_token: String,
}
