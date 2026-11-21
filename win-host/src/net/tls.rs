use crate::security::{write_protected_file, SDDL_SYSTEM_ADMIN_ONLY};
use anyhow::{anyhow, Context, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType, PKCS_ECDSA_P256_SHA256};
use rustls::ServerConfig;
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio_rustls::TlsAcceptor;

fn ensure_crypto_provider() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Clone)]
pub struct TlsMaterial {
    pub cert_pem: String,
    pub key_pem: String,
    pub fingerprint: String,
    pub short_fingerprint: String,
}

impl TlsMaterial {
    pub fn build_acceptor(&self) -> Result<TlsAcceptor> {
        ensure_crypto_provider();
        let certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(self.cert_pem.as_bytes())
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("parse TLS certificate PEM")?;
        if certs.is_empty() {
            return Err(anyhow!("TLS certificate PEM contains no certificates"));
        }
        let key = PrivateKeyDer::from_pem_slice(self.key_pem.as_bytes())
            .context("parse TLS private key PEM")?;
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("build rustls ServerConfig")?;
        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }
}

/// Computes uppercase colon-separated SHA-256 fingerprint of a DER-encoded X.509 certificate.
pub fn sha256_fingerprint_der(der: &[u8]) -> (String, String) {
    let digest = Sha256::digest(der);
    let full = digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let short = digest[..8]
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join("");
    (full, short)
}

pub fn fingerprint_from_pem(cert_pem: &str) -> Result<(String, String)> {
    let cert = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .context("decode certificate PEM for fingerprint")?;
    Ok(sha256_fingerprint_der(cert.as_ref()))
}

pub fn generate_self_signed(host_name: &str) -> Result<TlsMaterial> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("WinBridge Host ({host_name})"));
    dn.push(DnType::OrganizationName, "WinBridge Local");
    params.distinguished_name = dn;
    params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().context("SAN localhost")?),
        SanType::IpAddress(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        SanType::IpAddress(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
    ];
    if let Ok(dns) = host_name.to_ascii_lowercase().try_into() {
        params.subject_alt_names.push(SanType::DnsName(dns));
    }
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .context("generate ECDSA P-256 keypair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("self-sign TLS certificate")?;
    let der = cert.der();
    let (fingerprint, short_fingerprint) = sha256_fingerprint_der(der.as_ref());
    Ok(TlsMaterial {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
        fingerprint,
        short_fingerprint,
    })
}

pub fn from_pem_pair(cert_pem: String, key_pem: String) -> Result<TlsMaterial> {
    let (fingerprint, short_fingerprint) = fingerprint_from_pem(&cert_pem)?;
    Ok(TlsMaterial {
        cert_pem,
        key_pem,
        fingerprint,
        short_fingerprint,
    })
}

/// Loads an existing persistent TLS certificate/key pair from `dir`, or generates and persists a
/// new self-signed ECDSA P-256 identity. When `restrict_system_admin` is true, locks down `key.pem`
/// with a SYSTEM+Administrators-only Windows DACL.
pub fn load_or_create(dir: &Path, restrict_system_admin: bool) -> Result<TlsMaterial> {
    let cert_path: PathBuf = dir.join("cert.pem");
    let key_path: PathBuf = dir.join("key.pem");
    if cert_path.exists() && key_path.exists() {
        if let (Ok(cert_pem), Ok(key_pem)) = (
            std::fs::read_to_string(&cert_path),
            std::fs::read_to_string(&key_path),
        ) {
            if let Ok(mat) = from_pem_pair(cert_pem, key_pem) {
                if mat.build_acceptor().is_ok() {
                    return Ok(mat);
                }
            }
        }
        tracing::warn!(
            dir = %dir.display(),
            "existing TLS certificate/key pair invalid; regenerating"
        );
    }

    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "winbridge-host".into());
    let mat = generate_self_signed(&host)?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create TLS directory {}", dir.display()))?;
    std::fs::write(&cert_path, mat.cert_pem.as_bytes())
        .with_context(|| format!("write {}", cert_path.display()))?;
    if restrict_system_admin {
        write_protected_file(&key_path, mat.key_pem.as_bytes(), SDDL_SYSTEM_ADMIN_ONLY)?;
    } else {
        std::fs::write(&key_path, mat.key_pem.as_bytes())
            .with_context(|| format!("write {}", key_path.display()))?;
    }
    Ok(mat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_self_signed_tls_acceptor_and_fingerprint() {
        let mat = generate_self_signed("TEST-PC").unwrap();
        assert_eq!(mat.fingerprint.len(), 95); // 32 bytes * 2 hex + 31 colons
        assert_eq!(mat.short_fingerprint.len(), 16);
        let acceptor = mat.build_acceptor().unwrap();
        drop(acceptor);
        let reparsed = from_pem_pair(mat.cert_pem.clone(), mat.key_pem.clone()).unwrap();
        assert_eq!(reparsed.fingerprint, mat.fingerprint);
    }
}
