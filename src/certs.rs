use std::path::PathBuf;

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

use crate::config::Config;

/// Certificate + key for HTTPS: use the configured files if set, otherwise
/// use (or create on first start) a self-signed pair under `certificates/`.
pub fn load_or_generate(cfg: &Config) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let (cert_path, key_path) = paths(cfg);
    if !cert_path.is_file() || !key_path.is_file() {
        if cfg.tls.cert_path.is_some() || cfg.tls.key_path.is_some() {
            anyhow::bail!(
                "configured certificate or key not found: {} / {}",
                cert_path.display(),
                key_path.display()
            );
        }
        generate_self_signed(&cert_path, &key_path)?;
    }
    load(&cert_path, &key_path)
}

pub fn paths(cfg: &Config) -> (PathBuf, PathBuf) {
    match (&cfg.tls.cert_path, &cfg.tls.key_path) {
        (Some(c), Some(k)) => (c.clone(), k.clone()),
        _ => {
            let dir = cfg.certificates_dir();
            (dir.join("cert.pem"), dir.join("key.pem"))
        }
    }
}

fn load(
    cert_path: &PathBuf,
    key_path: &PathBuf,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("opening certificate {}", cert_path.display()))?;
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut std::io::BufReader::new(cert_file))
            .collect::<Result<_, _>>()
            .context("parsing certificate")?;
    if certs.is_empty() {
        anyhow::bail!("no certificate found in {}", cert_path.display());
    }
    let key_file = std::fs::File::open(key_path)
        .with_context(|| format!("opening private key {}", key_path.display()))?;
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(key_file))
        .context("parsing private key")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {}", key_path.display()))?;
    Ok((certs, key))
}

/// Self-signed certificate valid for the local host names and all current
/// local IP addresses, so Cisco devices can pin/verify by IP.
fn generate_self_signed(cert_path: &PathBuf, key_path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut names: Vec<String> = vec!["localhost".into(), "transferbuddy.local".into()];
    for ifa in crate::netif::interfaces() {
        names.push(ifa.ip.to_string());
    }
    names.dedup();

    let key_pair = rcgen::KeyPair::generate().context("generating key")?;
    let mut params = rcgen::CertificateParams::new(names).context("certificate params")?;
    params.distinguished_name = {
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "transferbuddy");
        dn
    };
    let cert = params.self_signed(&key_pair).context("self-signing certificate")?;

    if let Some(dir) = cert_path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(cert_path, cert.pem())
        .with_context(|| format!("writing {}", cert_path.display()))?;
    let _ = std::fs::remove_file(key_path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    use std::io::Write;
    let mut f = opts.open(key_path).with_context(|| format!("writing {}", key_path.display()))?;
    f.write_all(key_pair.serialize_pem().as_bytes())?;
    Ok(())
}

pub struct CertInfo {
    pub path: PathBuf,
    pub fingerprint_sha256: String,
}

/// SHA-256 fingerprint (colon-separated) of the active certificate, if any.
pub fn info(cfg: &Config) -> Option<CertInfo> {
    let (cert_path, key_path) = paths(cfg);
    let (certs, _) = load(&cert_path, &key_path).ok()?;
    let der = certs.first()?;
    let digest = Sha256::digest(der.as_ref());
    let fingerprint = digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let _ = key_path;
    Some(CertInfo { path: cert_path, fingerprint_sha256: fingerprint })
}
