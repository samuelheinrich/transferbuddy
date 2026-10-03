use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Fixed default credentials. Cisco devices are provisioned by hand at a
/// console, so a well-known pair beats anything random or per-run. A different
/// password is only ever used when it was set explicitly — in the TUI
/// (w / service edit), with `--password`, or by editing the secrets file.
pub const DEFAULT_USERNAME: &str = "cisco";
pub const DEFAULT_PASSWORD: &str = "cisco123";

/// Load the explicitly stored password, or fall back to [`DEFAULT_PASSWORD`].
/// Passwords are never generated: an unattended start always yields
/// `cisco / cisco123`.
pub fn load_password(secrets_path: &Path) -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(secrets_path) {
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("password = ") {
                let v = v.trim().trim_matches('"');
                if !v.is_empty() {
                    return Ok(v.to_string());
                }
            }
        }
    }
    // No stored password yet — seed the well-known default and persist it.
    store_password(secrets_path, DEFAULT_PASSWORD)?;
    Ok(DEFAULT_PASSWORD.to_string())
}

pub fn store_password(secrets_path: &Path, password: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = secrets_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(secrets_path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    let mut f = opts
        .open(secrets_path)
        .with_context(|| format!("creating {}", secrets_path.display()))?;
    use std::io::Write;
    writeln!(f, "# transferbuddy secrets — never commit this file")?;
    writeln!(f, "password = \"{password}\"")?;
    Ok(())
}

fn hash(s: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().into()
}

/// Username/password verification with per-IP lockout after repeated
/// failures. Comparison is done on SHA-256 digests (fixed length, so timing
/// does not leak password length or content).
pub struct Authenticator {
    user_hash: [u8; 32],
    pass_hash: [u8; 32],
    max_failures: u32,
    lockout: Duration,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthResult {
    Ok,
    BadCredentials,
    LockedOut,
}

impl Authenticator {
    pub fn new(username: &str, password: &str, max_failures: u32, lockout_secs: u64) -> Self {
        Self {
            user_hash: hash(username),
            pass_hash: hash(password),
            max_failures: max_failures.max(1),
            lockout: Duration::from_secs(lockout_secs.max(1)),
            failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, ip: IpAddr, username: &str, password: &str) -> AuthResult {
        {
            let mut failures = self.failures.lock().unwrap();
            if let Some((count, since)) = failures.get(&ip) {
                if *count >= self.max_failures {
                    if since.elapsed() < self.lockout {
                        return AuthResult::LockedOut;
                    }
                    failures.remove(&ip);
                }
            }
        }
        let user_ok = hash(username) == self.user_hash;
        let pass_ok = hash(password) == self.pass_hash;
        if user_ok && pass_ok {
            self.failures.lock().unwrap().remove(&ip);
            AuthResult::Ok
        } else {
            let mut failures = self.failures.lock().unwrap();
            let entry = failures.entry(ip).or_insert((0, Instant::now()));
            entry.0 += 1;
            entry.1 = Instant::now();
            AuthResult::BadCredentials
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip() -> IpAddr {
        "192.0.2.9".parse().unwrap()
    }

    #[test]
    fn accepts_valid_credentials() {
        let a = Authenticator::new("cisco", "secret", 5, 60);
        assert_eq!(a.check(ip(), "cisco", "secret"), AuthResult::Ok);
    }

    #[test]
    fn rejects_bad_credentials() {
        let a = Authenticator::new("cisco", "secret", 5, 60);
        assert_eq!(a.check(ip(), "cisco", "wrong"), AuthResult::BadCredentials);
        assert_eq!(
            a.check(ip(), "nobody", "secret"),
            AuthResult::BadCredentials
        );
    }

    #[test]
    fn lockout_after_failures_and_reset_on_success() {
        let a = Authenticator::new("cisco", "secret", 3, 60);
        for _ in 0..3 {
            assert_eq!(a.check(ip(), "cisco", "wrong"), AuthResult::BadCredentials);
        }
        assert_eq!(a.check(ip(), "cisco", "secret"), AuthResult::LockedOut);
        // A different IP is unaffected.
        let other: IpAddr = "192.0.2.10".parse().unwrap();
        assert_eq!(a.check(other, "cisco", "secret"), AuthResult::Ok);
    }

    #[test]
    fn password_defaults_to_cisco123_and_is_never_generated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/secrets.toml");
        // First run seeds the well-known default and persists it.
        assert_eq!(load_password(&path).unwrap(), "cisco123");
        // Every following run returns exactly the same password.
        assert_eq!(load_password(&path).unwrap(), "cisco123");
        // An explicitly stored password wins over the default.
        store_password(&path, "n3wpass").unwrap();
        assert_eq!(load_password(&path).unwrap(), "n3wpass");
    }

    #[test]
    fn password_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/secrets.toml");
        let p1 = load_password(&path).unwrap();
        assert_eq!(p1, DEFAULT_PASSWORD);
        assert_eq!(load_password(&path).unwrap(), p1);
        // Restrictive permissions.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
