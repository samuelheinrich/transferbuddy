use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub root: Option<PathBuf>,
    pub size: [f32; 2],
    pub dark: Option<bool>,
    pub split: f32,
    pub compact: bool,
    pub scale: f32,
    #[serde(default)]
    pub appearance_version: u8,
    pub reduce_motion: bool,
    pub devices_height: f32,
    pub files_height: f32,
    pub console_height: f32,
    pub columns: std::collections::BTreeMap<String, Vec<f32>>,
    pub devices_collapsed: bool,
    pub jobs_height: f32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            root: None,
            size: [1440.0, 900.0],
            dark: Some(true),
            split: 0.48,
            compact: true,
            scale: 0.85,
            appearance_version: 1,
            reduce_motion: false,
            devices_height: 400.0,
            files_height: 260.0,
            console_height: 240.0,
            columns: Default::default(),
            devices_collapsed: false,
            jobs_height: 180.0,
        }
    }
}
impl Preferences {
    pub fn load(dir: &Path) -> Self {
        let mut prefs: Self = std::fs::read_to_string(dir.join("desktop.toml"))
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        if prefs.appearance_version == 0 {
            if prefs.scale == 1.0 && !prefs.compact {
                prefs.scale = 0.85;
                prefs.compact = true;
            }
            prefs.appearance_version = 1;
        }
        for value in &mut prefs.size {
            if !value.is_finite() || *value < 600.0 || *value > 16000.0 {
                *value = 1000.0
            }
        }
        prefs.split = if prefs.split.is_finite() {
            prefs.split.clamp(0.2, 0.8)
        } else {
            0.48
        };
        prefs.scale = finite_range(prefs.scale, 0.85, 0.85, 2.0);
        prefs.devices_height = finite_range(prefs.devices_height, 400.0, 100.0, 600.0);
        prefs.files_height = finite_range(prefs.files_height, 260.0, 120.0, 600.0);
        prefs.console_height = finite_range(prefs.console_height, 240.0, 120.0, 900.0);
        prefs.jobs_height = finite_range(prefs.jobs_height, 180.0, 100.0, 500.0);
        prefs.columns.retain(|key, widths| {
            key.len() < 80
                && widths.len() <= 12
                && widths
                    .iter()
                    .all(|w| w.is_finite() && *w >= 40.0 && *w <= 4000.0)
        });
        prefs
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!("desktop.{}.tmp", std::process::id()));
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(tmp, dir.join("desktop.toml")).map_err(|e| e.to_string())
    }
}
fn finite_range(value: f32, fallback: f32, low: f32, high: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        fallback
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remembered_root_and_window_do_not_store_credentials() {
        let d = tempfile::tempdir().unwrap();
        let p = Preferences {
            root: Some(d.path().into()),
            ..Default::default()
        };
        p.save(d.path()).unwrap();
        let loaded = Preferences::load(d.path());
        assert_eq!(loaded.root, p.root);
        let s = std::fs::read_to_string(d.path().join("desktop.toml")).unwrap();
        assert!(!s.contains("password"));
    }
    #[test]
    fn old_preferences_get_console_defaults() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("desktop.toml"),
            "size = [1000, 700]\nsplit = 0.6\n",
        )
        .unwrap();
        let p = Preferences::load(d.path());
        assert_eq!(p.size, [1000.0, 700.0]);
        assert_eq!(p.split, 0.6);
        assert_eq!(p.scale, 0.85);
        assert!(p.compact);
        assert_eq!(p.dark, Some(true));
        assert_eq!(p.devices_height, 400.0);
    }
    #[test]
    fn saved_preferences_preserve_an_explicit_standard_layout() {
        let d = tempfile::tempdir().unwrap();
        Preferences {
            scale: 1.0,
            compact: false,
            ..Default::default()
        }
        .save(d.path())
        .unwrap();
        let loaded = Preferences::load(d.path());
        assert_eq!(loaded.scale, 1.0);
        assert!(!loaded.compact);
        std::fs::write(
            d.path().join("desktop.toml"),
            "scale = 1.0\ncompact = false\n",
        )
        .unwrap();
        let migrated = Preferences::load(d.path());
        assert_eq!(migrated.scale, 0.85);
        assert!(migrated.compact);
    }
    #[test]
    fn invalid_layout_values_cannot_poison_the_renderer() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Preferences {
            scale: f32::NAN,
            split: f32::INFINITY,
            devices_height: f32::NAN,
            ..Default::default()
        };
        p.columns.insert("bad".into(), vec![f32::NAN]);
        p.columns.insert("valid".into(), vec![180.0, 120.0]);
        p.save(d.path()).unwrap();
        let loaded = Preferences::load(d.path());
        assert_eq!(loaded.scale, 0.85);
        assert_eq!(loaded.split, 0.48);
        assert_eq!(loaded.devices_height, 400.0);
        assert!(!loaded.columns.contains_key("bad"));
        assert!(loaded.columns.contains_key("valid"));
    }
}
