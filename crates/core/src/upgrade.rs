use std::io::Read;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub enum Progress {
    #[default]
    Idle,
    Copying,
    Verifying,
    Verified {
        remote: String,
        md5: String,
        version: Option<String>,
    },
    Installing,
    AwaitingReload {
        prompt: String,
    },
    Rebooting {
        since: Instant,
        attempts: u32,
        last_error: Option<String>,
    },
    Complete {
        version: String,
    },
    Failed(String),
}
impl Progress {
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "not deployed".into(),
            Self::Copying => "copying".into(),
            Self::Verifying => "checking MD5".into(),
            Self::Verified {
                version: Some(_), ..
            } => "MD5 verified — ready to install".into(),
            Self::Verified { version: None, .. } => "MD5 verified — transfer complete".into(),
            Self::Installing => "installing".into(),
            Self::AwaitingReload { .. } => "confirm reload in console".into(),
            Self::Rebooting {
                since, attempts, ..
            } => format!(
                "rebooting {:02}:{:02} — login attempt {attempts}",
                since.elapsed().as_secs() / 60,
                since.elapsed().as_secs() % 60
            ),
            Self::Complete { version } => format!("upgrade successful: {version}"),
            Self::Failed(error) => format!("failed: {error}"),
        }
    }
}

/// Shared UI/backend gate. Facts are refreshed again immediately before install.
pub fn install_blocker(progress: &Progress, info: &crate::cisco::VersionInfo) -> Option<String> {
    let Progress::Verified {
        version: Some(expected),
        ..
    } = progress
    else {
        return Some(
            "upload or verify an existing full release image; local and remote MD5 must match"
                .into(),
        );
    };
    let Some(running) = info.version.as_ref() else {
        return Some("read the running version before upgrading".into());
    };
    if normalize_version(running) == normalize_version(expected) {
        return Some(format!("version {running} is already installed"));
    }
    None
}

pub fn local_md5(path: &Path) -> anyhow::Result<String> {
    use md5::{Digest, Md5};
    let mut file = std::fs::File::open(path)?;
    let mut digest = Md5::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn remote_md5(output: &str) -> Option<String> {
    // The hash in an echoed verify command must never count as device evidence.
    output
        .lines()
        .filter(|l| l.contains('=') && l.to_ascii_lowercase().contains("md5"))
        .flat_map(|l| l.rsplit_once('=').map(|(_, result)| result).into_iter())
        .flat_map(|l| l.split(|c: char| !c.is_ascii_hexdigit()))
        .find(|part| part.len() == 32)
        .map(str::to_ascii_lowercase)
}

pub fn image_version(name: &str) -> Option<String> {
    let name = name.rsplit('/').next()?;
    if !name.to_ascii_lowercase().ends_with(".bin")
        || name.to_ascii_lowercase().ends_with(".smu.bin")
    {
        return None;
    }
    let pieces: Vec<_> = name.split(['.', '_']).collect();
    pieces.windows(3).find_map(|p| {
        if p[0].parse::<u32>().is_err()
            || p[1].parse::<u32>().is_err()
            || !p[2].starts_with(|c: char| c.is_ascii_digit())
        {
            return None;
        }
        Some(normalize_version(&p.join(".")))
    })
}
pub fn normalize_version(version: &str) -> String {
    version
        .split('.')
        .map(|part| {
            let digits = part.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits == 0 {
                return part.to_ascii_lowercase();
            }
            format!(
                "{}{}",
                part[..digits].parse::<u32>().unwrap_or(0),
                part[digits..].to_ascii_lowercase()
            )
        })
        .collect::<Vec<_>>()
        .join(".")
}

pub fn ios_file(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "packages.conf"
        || ((name.starts_with("cat9k") || name.starts_with("cat3k") || name.starts_with("c9800"))
            && [".bin", ".pkg", ".conf"]
                .iter()
                .any(|suffix| name.ends_with(suffix)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installation_requires_verified_new_release_and_known_current_version() {
        let progress = Progress::Verified {
            remote: "flash:one.bin".into(),
            md5: "x".into(),
            version: Some("17.15.6".into()),
        };
        let mut info = crate::cisco::VersionInfo {
            version: Some("17.15.03".into()),
            ..Default::default()
        };
        assert!(install_blocker(&progress, &info).is_none());
        info.version = Some("17.15.06".into());
        assert!(install_blocker(&progress, &info)
            .unwrap()
            .contains("already installed"));
        info.version = None;
        assert!(install_blocker(&progress, &info).is_some());
        assert!(install_blocker(&Progress::Idle, &info).is_some());
    }

    #[test]
    fn checksum_requires_device_result_and_streams_local_file() {
        let digest = "78805a221a988e79ef3f42d7c5bfd418";
        assert_eq!(
            remote_md5(&format!("verify /md5 flash:a.bin {digest}")),
            None
        );
        assert_eq!(
            remote_md5(&format!(
                "verify /md5 (flash:a.bin) = {}",
                digest.to_ascii_uppercase()
            )),
            Some(digest.into())
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        std::fs::write(&file, b"image").unwrap();
        assert_eq!(local_md5(&file).unwrap(), digest);
    }
    #[test]
    fn release_images_and_ios_artifacts_are_recognised() {
        for file in [
            "cat9k_iosxe.17.15.03.SPA.bin",
            "C9800-CL-universalk9.17.15.03.SPA.bin",
        ] {
            assert_eq!(image_version(file).as_deref(), Some("17.15.3"));
            assert!(ios_file(file));
        }
        assert_eq!(
            image_version("cat9k_lite_iosxe.17.09.04a.SPA.bin").as_deref(),
            Some("17.9.4a")
        );
        for file in [
            "packages.conf",
            "cat9k_lite-rpbase.17.15.03.SPA.pkg",
            "cat9k_lite_iosxe.17.15.03.SPA.conf",
        ] {
            assert!(ios_file(file));
            assert!(image_version(file).is_none());
        }
        assert!(image_version("cat9k_iosxe.17.15.03.smu.bin").is_none());
        assert!(!ios_file("backup.zip"));
    }
}
