//! Persistent user settings, stored as TOML in the user's config directory.
//!
//! Precedence at runtime: command-line flag, then this file, then the default.
//! The file only ever holds the user's own choices; values forced by a
//! command-line flag are never written back.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name of the accent color preset (e.g. "Amber")
    pub accent: String,
    /// Swap left/right of the timecode input
    pub swap_input: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            accent: "Amber".to_string(),
            swap_input: false,
        }
    }
}

impl Settings {
    /// `$XDG_CONFIG_HOME/neowax/settings.toml`, or `~/.config/neowax/settings.toml`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("neowax").join("settings.toml"))
    }

    /// Load settings, falling back to defaults. A missing file is normal; an
    /// unreadable or invalid one is moved aside (`.broken`) so a later save
    /// doesn't silently destroy the user's edits.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("Cannot read {:?}: {}", path, e);
                return Self::default();
            }
        };
        match toml::from_str(&text) {
            Ok(settings) => settings,
            Err(e) => {
                let broken = path.with_extension("toml.broken");
                tracing::error!(
                    "Invalid settings in {:?} ({}); moving to {:?}",
                    path,
                    e,
                    broken
                );
                std::fs::rename(path, broken).ok();
                Self::default()
            }
        }
    }

    /// Write the settings atomically (temp file + rename), so a power cut
    /// can't leave a truncated file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("settings path has no parent")?;
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {dir:?}"))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)
            .with_context(|| format!("cannot write {tmp:?}"))?;
        std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {path:?}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("neowax-test-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn roundtrip() {
        let dir = scratch_dir("roundtrip");
        let path = dir.join("nested").join("settings.toml");
        let settings = Settings {
            accent: "Cyan".to_string(),
            swap_input: true,
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_gives_defaults() {
        let path = scratch_dir("missing").join("settings.toml");
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn partial_and_unknown_keys_are_tolerated() {
        let dir = scratch_dir("partial");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(&path, "swap_input = true\nfuture_option = 3\n").unwrap();
        let loaded = Settings::load(&path);
        assert!(loaded.swap_input);
        assert_eq!(loaded.accent, Settings::default().accent);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn invalid_file_is_moved_aside() {
        let dir = scratch_dir("invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(&path, "this is = not [valid").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(!path.exists());
        assert!(dir.join("settings.toml.broken").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
