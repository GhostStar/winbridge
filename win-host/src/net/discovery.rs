use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

const SERVICE_TYPE: &str = "_winbridge._tcp.local.";

/// Advertises the WinBridge Windows host over LAN mDNS / Bonjour (`_winbridge._tcp.local.`)
/// so the macOS client discovers it automatically without manually typing an IP address.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    pub fn start(port: u16, tls_enabled: bool, short_fingerprint: Option<&str>) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "WinBridge-Host".into());
        let clean_host: String = host
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let host_label = if clean_host.is_empty() {
            "winbridge-host".to_owned()
        } else {
            clean_host.to_ascii_lowercase()
        };
        let host_name = format!("{host_label}.local.");

        let mut props: HashMap<String, String> = HashMap::new();
        props.insert("v".into(), "1".into());
        props.insert("tls".into(), if tls_enabled { "1" } else { "0" }.into());
        if let Some(fp) = short_fingerprint {
            props.insert("fp".into(), fp.to_owned());
        }

        let info = ServiceInfo::new(SERVICE_TYPE, &host, &host_name, "", port, props)?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        tracing::info!(
            service = %fullname,
            port,
            tls = tls_enabled,
            "mDNS LAN discovery advertiser started"
        );
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}
