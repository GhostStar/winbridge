use crate::config::{CaptureMode, Codec, Config};
use anyhow::{anyhow, bail, ensure, Context, Result};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

/// SDDL granting full access ONLY to LocalSystem (`SY`) and Built-in Administrators (`BA`),
/// blocking inheritance (`P`) from parent folders.
pub const SDDL_SYSTEM_ADMIN_ONLY: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)";

/// Constant-time byte comparison to prevent timing side-channels when validating auth tokens.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len_diff = a.len() ^ b.len();
    let max_len = a.len().max(b.len());
    let mut diff = len_diff;
    for i in 0..max_len {
        let ba = if i < a.len() { a[i] } else { 0 };
        let bb = if i < b.len() { b[i] } else { 0 };
        diff |= usize::from(ba ^ bb);
    }
    diff == 0
}

/// Generates a URL-safe cryptographically random 32-character (192-bit) auth token.
pub fn generate_token() -> Result<String> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow!("OS CSPRNG failed while generating auth token"))?;
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push(ALPHABET[(b & 0x3F) as usize] as char);
    }
    Ok(out)
}

/// Canonical ProgramData directory for service configuration, TLS certs, and logs.
pub fn program_data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
        .join("WinBridge")
}

/// User-writable fallback directory for standalone (non-service) TLS keys.
pub fn user_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".winbridge")))
        .unwrap_or_else(std::env::temp_dir)
        .join("WinBridge")
}

pub fn service_config_path() -> PathBuf {
    program_data_dir().join("service.conf")
}

/// Applies an explicit SDDL DACL to an existing file or directory on Windows.
#[cfg(windows)]
pub fn apply_path_sddl(path: &Path, sddl: &str) -> Result<()> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{LocalFree, ERROR_SUCCESS},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
                SE_FILE_OBJECT,
            },
            GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
    };

    let sddl_w: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut path_w: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();

    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_w.as_ptr(),
            1, // SDDL_REVISION_1
            &mut sd,
            ptr::null_mut(),
        ) == 0
        {
            bail!(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW failed: {}",
                io::Error::last_os_error()
            );
        }
        let mut dacl_present = 0;
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut dacl_defaulted = 0;
        let ok = GetSecurityDescriptorDacl(sd, &mut dacl_present, &mut dacl, &mut dacl_defaulted);
        if ok == 0 {
            let err = io::Error::last_os_error();
            LocalFree(sd);
            bail!("GetSecurityDescriptorDacl failed: {err}");
        }
        let status = SetNamedSecurityInfoW(
            path_w.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null_mut(),
        );
        LocalFree(sd);
        if status != ERROR_SUCCESS {
            bail!(
                "SetNamedSecurityInfoW('{}') failed with Win32 status {}",
                path.display(),
                status
            );
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn apply_path_sddl(_path: &Path, _sddl: &str) -> Result<()> {
    Ok(())
}

/// Writes `content` to `path`, creating parent directories and restricting the file's Windows DACL
/// to `sddl` (e.g. `SYSTEM` + `Administrators` only).
pub fn write_protected_file(path: &Path, content: &[u8], sddl: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    apply_path_sddl(path, sddl)
        .with_context(|| format!("restrict ACL on {}", path.display()))?;
    Ok(())
}

/// Serializes persistent Windows Service settings into key=value format stored inside
/// `C:\ProgramData\WinBridge\service.conf` (never in SCM ImagePath or process CLI args).
pub fn serialize_service_config(config: &Config) -> String {
    let codec = match config.codec {
        Codec::Auto => "auto",
        Codec::Hevc => "hevc",
        Codec::H264 => "h264",
        Codec::Jpeg => "jpeg",
        Codec::Raw => "raw",
    };
    let capture_mode = match config.capture_mode {
        CaptureMode::Window => "window",
        CaptureMode::MonitorCrop => "monitor-crop",
    };
    let mut lines = vec![
        "# WinBridge Service Configuration (SYSTEM/Administrators only)".to_owned(),
        format!("bind={}", config.bind),
        format!("port={}", config.port),
        format!("token={}", config.token.replace(['\r', '\n'], "")),
        format!("fps={}", config.fps),
        format!("bitrate={}", config.bitrate),
        format!("codec={codec}"),
        format!("capture_mode={capture_mode}"),
        format!("capture_cursor={}", config.capture_cursor),
        format!("log_level={}", config.log_level.replace(['\r', '\n'], "")),
        format!("no_tls={}", config.no_tls),
        format!(
            "allow_insecure_no_auth={}",
            config.allow_insecure_no_auth
        ),
        format!("no_discovery={}", config.no_discovery),
    ];
    for cidr in &config.allow_cidr {
        lines.push(format!("allow_cidr={}", cidr.replace(['\r', '\n'], "")));
    }
    lines.join("\n") + "\n"
}

/// Applies key=value overrides loaded from `service.conf` onto a base `Config`.
pub fn apply_service_config_text(config: &mut Config, text: &str) -> Result<()> {
    let mut cidrs = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "bind" => config.bind = val.parse().with_context(|| format!("invalid bind '{val}'"))?,
            "port" => config.port = val.parse().with_context(|| format!("invalid port '{val}'"))?,
            "token" => config.token = val.to_owned(),
            "fps" => {
                let f: u32 = val.parse().with_context(|| format!("invalid fps '{val}'"))?;
                config.fps = f.clamp(1, 240);
            }
            "bitrate" => {
                let b: u32 = val
                    .parse()
                    .with_context(|| format!("invalid bitrate '{val}'"))?;
                config.bitrate = b.clamp(1, 4_294_967);
            }
            "codec" => {
                config.codec = match val {
                    "auto" => Codec::Auto,
                    "hevc" => Codec::Hevc,
                    "h264" => Codec::H264,
                    "jpeg" => Codec::Jpeg,
                    "raw" => Codec::Raw,
                    other => bail!("unknown codec '{other}' in service.conf"),
                };
            }
            "capture_mode" => {
                config.capture_mode = match val {
                    "window" => CaptureMode::Window,
                    "monitor-crop" => CaptureMode::MonitorCrop,
                    other => bail!("unknown capture_mode '{other}' in service.conf"),
                };
            }
            "capture_cursor" => config.capture_cursor = val == "true" || val == "1",
            "log_level" if !val.is_empty() => config.log_level = val.to_owned(),
            "no_tls" => config.no_tls = val == "true" || val == "1",
            "allow_insecure_no_auth" => {
                config.allow_insecure_no_auth = val == "true" || val == "1"
            }
            "no_discovery" => config.no_discovery = val == "true" || val == "1",
            "allow_cidr" if !val.is_empty() => cidrs.push(val.to_owned()),
            _ => {}
        }
    }
    if !cidrs.is_empty() {
        config.allow_cidr = cidrs;
    }
    Ok(())
}

pub fn save_service_config(config: &Config) -> Result<PathBuf> {
    let path = service_config_path();
    let body = serialize_service_config(config);
    write_protected_file(&path, body.as_bytes(), SDDL_SYSTEM_ADMIN_ONLY)?;
    Ok(path)
}

pub fn load_service_config(base: &mut Config) -> Result<bool> {
    let path = service_config_path();
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read service config {}", path.display()))?;
    apply_service_config_text(base, &text)?;
    Ok(true)
}

/// Secrets transferred from Session 0 `--service` to Session 1 `--session-worker` over an
/// anonymous stdin pipe (never exposed via process command-line arguments).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerSecrets {
    pub auth_token: String,
    pub tls_cert_pem: String,
    pub tls_key_pem: String,
}

impl WorkerSecrets {
    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for field in [&self.auth_token, &self.tls_cert_pem, &self.tls_key_pem] {
            let b = field.as_bytes();
            out.extend_from_slice(&(b.len() as u32).to_be_bytes());
            out.extend_from_slice(b);
        }
        out
    }

    pub fn decode_from_reader(reader: &mut impl io::Read) -> Result<Self> {
        fn read_str(r: &mut impl io::Read) -> Result<String> {
            let mut len_buf = [0u8; 4];
            r.read_exact(&mut len_buf)?;
            let len = u32::from_be_bytes(len_buf) as usize;
            ensure!(len <= 1024 * 1024, "secret field too large ({len} bytes)");
            let mut buf = vec![0u8; len];
            r.read_exact(&mut buf)?;
            String::from_utf8(buf).context("non-UTF8 secret field")
        }
        Ok(Self {
            auth_token: read_str(reader)?,
            tls_cert_pem: read_str(reader)?,
            tls_key_pem: read_str(reader)?,
        })
    }
}

// ============================================================================
// Per-IP Authentication Rate Limiter (Exponential Backoff)
// ============================================================================

struct FailureRecord {
    failures: u32,
    locked_until: Option<Instant>,
    last_seen: Instant,
}

#[derive(Default)]
pub struct AuthLimiter {
    records: Mutex<HashMap<IpAddr, FailureRecord>>,
}

impl AuthLimiter {
    /// Checks whether `ip` is currently rate-limited; returns `Err(remaining)` if locked out.
    pub fn check(&self, ip: IpAddr, now: Instant) -> std::result::Result<(), Duration> {
        let mut map = self.records.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, r| now.saturating_duration_since(r.last_seen) < Duration::from_secs(600));
        if let Some(rec) = map.get(&ip) {
            if let Some(until) = rec.locked_until {
                if until > now {
                    return Err(until.duration_since(now));
                }
            }
        }
        Ok(())
    }

    /// Records a failed authentication attempt and returns any newly imposed lockout duration.
    pub fn record_failure(&self, ip: IpAddr, now: Instant) -> Duration {
        let mut map = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let rec = map.entry(ip).or_insert(FailureRecord {
            failures: 0,
            locked_until: None,
            last_seen: now,
        });
        rec.failures = rec.failures.saturating_add(1);
        rec.last_seen = now;
        let penalty = match rec.failures {
            0..=2 => Duration::ZERO,
            3 => Duration::from_secs(5),
            4 => Duration::from_secs(15),
            5 => Duration::from_secs(60),
            _ => Duration::from_secs(300),
        };
        if !penalty.is_zero() {
            rec.locked_until = Some(now + penalty);
        }
        penalty
    }

    pub fn record_success(&self, ip: IpAddr) {
        let mut map = self.records.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&ip);
    }
}

// ============================================================================
// Optional IP / CIDR Allowlist
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
enum CidrEntry {
    V4 { network: u32, mask: u32 },
    V6 { network: u128, mask: u128 },
}

#[derive(Clone, Debug, Default)]
pub struct IpAllowlist {
    entries: Vec<CidrEntry>,
}

impl IpAllowlist {
    pub fn parse(specs: &[String]) -> Result<Self> {
        let mut entries = Vec::with_capacity(specs.len());
        for raw in specs {
            let s = raw.trim();
            if s.is_empty() {
                continue;
            }
            if let Some((ip_str, prefix_str)) = s.split_once('/') {
                let ip: IpAddr = ip_str
                    .parse()
                    .with_context(|| format!("invalid CIDR IP '{ip_str}'"))?;
                let prefix: u32 = prefix_str
                    .parse()
                    .with_context(|| format!("invalid CIDR prefix '{prefix_str}'"))?;
                entries.push(Self::make_entry(ip, prefix)?);
            } else {
                let ip: IpAddr = s
                    .parse()
                    .with_context(|| format!("invalid IP address '{s}'"))?;
                let prefix = match ip {
                    IpAddr::V4(_) => 32,
                    IpAddr::V6(_) => 128,
                };
                entries.push(Self::make_entry(ip, prefix)?);
            }
        }
        Ok(Self { entries })
    }

    fn make_entry(ip: IpAddr, prefix: u32) -> Result<CidrEntry> {
        match ip {
            IpAddr::V4(v4) => {
                ensure!(prefix <= 32, "IPv4 CIDR prefix /{prefix} exceeds 32");
                let mask = if prefix == 0 {
                    0u32
                } else {
                    u32::MAX << (32 - prefix)
                };
                Ok(CidrEntry::V4 {
                    network: u32::from(v4) & mask,
                    mask,
                })
            }
            IpAddr::V6(v6) => {
                ensure!(prefix <= 128, "IPv6 CIDR prefix /{prefix} exceeds 128");
                let mask = if prefix == 0 {
                    0u128
                } else {
                    u128::MAX << (128 - prefix)
                };
                Ok(CidrEntry::V6 {
                    network: u128::from(v6) & mask,
                    mask,
                })
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        if self.entries.is_empty() {
            return true;
        }
        let normalized = match ip {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(v6),
            },
            v4 => v4,
        };
        self.entries.iter().any(|entry| match (entry, normalized) {
            (CidrEntry::V4 { network, mask }, IpAddr::V4(v4)) => (u32::from(v4) & *mask) == *network,
            (CidrEntry::V6 { network, mask }, IpAddr::V6(v6)) => {
                (u128::from(v6) & *mask) == *network
            }
            (CidrEntry::V4 { network, mask }, IpAddr::V6(v6)) => v6
                .to_ipv4_mapped()
                .is_some_and(|v4| (u32::from(v4) & *mask) == *network),
            (CidrEntry::V6 { .. }, IpAddr::V4(v4)) => {
                let mapped: Ipv6Addr = v4.to_ipv6_mapped();
                matches!(entry, CidrEntry::V6 { network, mask } if (u128::from(mapped) & *mask) == *network)
            }
        })
    }
}

/// Validates startup auth posture: non-loopback binds MUST configure an auth token unless
/// `--allow-insecure-no-auth` is explicitly passed.
pub fn validate_auth_policy(config: &Config) -> Result<()> {
    let is_loopback = match config.bind {
        IpAddr::V4(v4) => v4 == Ipv4Addr::LOCALHOST || v4.is_loopback(),
        IpAddr::V6(v6) => v6 == Ipv6Addr::LOCALHOST || v6.is_loopback(),
    };
    if config.token.is_empty() && !is_loopback && !config.allow_insecure_no_auth {
        bail!(
            "Refusing to bind non-loopback address {} without an auth token.\n\
             Pass --token <SECRET>, set WINBRIDGE_TOKEN, use --token-file <PATH>,\n\
             or pass --allow-insecure-no-auth only on a fully trusted isolated LAN.",
            config.bind
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn constant_time_eq_checks_content_and_length() {
        assert!(constant_time_eq(b"secret-123", b"secret-123"));
        assert!(!constant_time_eq(b"secret-123", b"secret-124"));
        assert!(!constant_time_eq(b"secret", b"secret-123"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn generates_strong_url_safe_tokens() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(b.len(), 32);
        assert_ne!(a, b);
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    #[test]
    fn auth_limiter_applies_exponential_backoff_and_resets_on_success() {
        let limiter = AuthLimiter::default();
        let ip: IpAddr = "192.168.1.50".parse().unwrap();
        let t0 = Instant::now();
        assert!(limiter.check(ip, t0).is_ok());
        assert_eq!(limiter.record_failure(ip, t0), Duration::ZERO);
        assert_eq!(limiter.record_failure(ip, t0), Duration::ZERO);
        assert_eq!(limiter.record_failure(ip, t0), Duration::from_secs(5));
        assert!(limiter.check(ip, t0 + Duration::from_secs(2)).is_err());
        assert!(limiter.check(ip, t0 + Duration::from_secs(6)).is_ok());
        assert_eq!(
            limiter.record_failure(ip, t0 + Duration::from_secs(6)),
            Duration::from_secs(15)
        );
        limiter.record_success(ip);
        assert!(limiter.check(ip, t0 + Duration::from_secs(7)).is_ok());
    }

    #[test]
    fn ip_allowlist_matches_ipv4_ipv6_and_mapped_addresses() {
        let list = IpAllowlist::parse(&[
            "192.168.1.0/24".into(),
            "100.64.0.0/10".into(),
            "127.0.0.1".into(),
            "fe80::/10".into(),
        ])
        .unwrap();
        assert!(list.allows("192.168.1.42".parse().unwrap()));
        assert!(list.allows("::ffff:192.168.1.42".parse().unwrap()));
        assert!(list.allows("100.100.1.5".parse().unwrap()));
        assert!(list.allows("127.0.0.1".parse().unwrap()));
        assert!(list.allows("fe80::1".parse().unwrap()));
        assert!(!list.allows("192.168.2.1".parse().unwrap()));
        assert!(!list.allows("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn worker_secrets_round_trip() {
        let secrets = WorkerSecrets {
            auth_token: "my-secret-token".into(),
            tls_cert_pem: "-----BEGIN CERTIFICATE-----\nABC\n-----END CERTIFICATE-----\n".into(),
            tls_key_pem: "-----BEGIN PRIVATE KEY-----\nXYZ\n-----END PRIVATE KEY-----\n".into(),
        };
        let raw = secrets.encode_bytes();
        let mut cursor = io::Cursor::new(raw);
        let decoded = WorkerSecrets::decode_from_reader(&mut cursor).unwrap();
        assert_eq!(decoded, secrets);
    }

    #[test]
    fn service_config_round_trip() {
        let mut cfg = Config::parse_from([
            "win-host",
            "--bind",
            "0.0.0.0",
            "--port",
            "9999",
            "--token",
            "super-secret",
            "--fps",
            "120",
            "--bitrate",
            "25000",
            "--codec",
            "h264",
            "--capture-mode",
            "monitor-crop",
            "--capture-cursor",
            "--allow-cidr",
            "192.168.1.0/24",
            "--allow-cidr",
            "10.0.0.0/8",
        ]);
        let serialized = serialize_service_config(&cfg);
        let mut loaded = Config::parse_from(["win-host"]);
        apply_service_config_text(&mut loaded, &serialized).unwrap();
        assert_eq!(loaded.bind, cfg.bind);
        assert_eq!(loaded.port, 9999);
        assert_eq!(loaded.token, "super-secret");
        assert_eq!(loaded.fps, 120);
        assert_eq!(loaded.bitrate, 25000);
        assert_eq!(loaded.codec, Codec::H264);
        assert_eq!(loaded.capture_mode, CaptureMode::MonitorCrop);
        assert!(loaded.capture_cursor);
        assert_eq!(loaded.allow_cidr, vec!["192.168.1.0/24", "10.0.0.0/8"]);
        cfg.token.clear();
        assert!(validate_auth_policy(&cfg).is_err());
        cfg.allow_insecure_no_auth = true;
        assert!(validate_auth_policy(&cfg).is_ok());
    }
}
