use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;

/// Load the SSH host key, generating an ed25519 key on first use.
/// The key file is created with 0600 permissions.
pub fn load_or_generate(cfg: &Config) -> Result<russh_keys::key::KeyPair> {
    let path = host_key_path(cfg);
    if path.is_file() {
        russh_keys::load_secret_key(&path, None)
            .with_context(|| format!("loading SSH host key {}", path.display()))
    } else {
        let key = russh_keys::key::KeyPair::generate_ed25519();
        save_key(&key, &path)?;
        Ok(key)
    }
}

pub fn host_key_path(cfg: &Config) -> PathBuf {
    cfg.ssh_dir().join("host_ed25519_key")
}

fn save_key(key: &russh_keys::key::KeyPair, path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    let f = opts
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    russh_keys::encode_pkcs8_pem(key, f).context("encoding SSH host key")?;
    Ok(())
}

/// SHA-256 fingerprint of the host key's public half.
pub fn fingerprint(cfg: &Config) -> Option<String> {
    let key = load_or_generate(cfg).ok()?;
    Some(key.clone_public_key().ok()?.fingerprint())
}
