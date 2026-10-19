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
    /// Show a scrolling close-up under a small overview of the track
    pub waveform_zoom: bool,
    /// Seconds of track the close-up shows (see `WAVEFORM_WINDOWS`)
    pub waveform_window: u32,
    /// Screen brightness in percent (see `BRIGHTNESS_LEVELS`)
    pub brightness: u8,
    /// Follow the timecode. When off, selecting a track plays it straight away.
    pub timecode_enabled: bool,

    // The settings below are only read at startup; changing them needs a restart.
    /// ALSA capture device (e.g. "plughw:sndrpihifiberry")
    pub input_device: String,
    /// ALSA playback device
    pub output_device: String,
    /// Look for new releases on GitHub (needs internet; off unless enabled)
    pub online_updates: bool,
    /// Directory scanned for music
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library: Option<PathBuf>,
    /// Timecode format (see `TimecodeDef::NAMES`)
    pub timecode: String,
    pub sample_rate: u32,
    /// Vinyl speed multiplier (1.0 = 33 rpm timecode, see `SPEEDS`)
    pub speed: f64,
    /// Phono-level input
    pub phono: bool,
    /// ALSA playback buffer size in frames
    pub buffer: u32,
    /// ALSA capture buffer size in frames
    pub capture_buffer: u32,
    /// CPU core the capture thread is pinned to
    pub capture_cpu: usize,
    /// CPU core the playback thread is pinned to
    pub playback_cpu: usize,
}

/// Brightness steps offered in the settings panel. The lowest is well above
/// zero so the screen can't be dimmed to black and lock the user out.
pub const BRIGHTNESS_LEVELS: &[u8] = &[
    10, 15, 20, 25, 30, 35, 40, 45, 50, 55, 60, 65, 70, 75, 80, 85, 90, 95, 100,
];

/// Close-up lengths offered for the zoomed waveform, in seconds.
pub const WAVEFORM_WINDOWS: &[u32] = &[8, 16, 32];

/// Values offered in the settings panel.
pub const SAMPLE_RATES: &[u32] = &[44100, 48000, 96000];
pub const PLAYBACK_BUFFERS: &[u32] = &[64, 128, 256, 512, 1024, 2048];
pub const CAPTURE_BUFFERS: &[u32] = &[256, 512, 1024, 2048, 4096];
/// Timecode speed: (label, multiplier). A 45 rpm record on a 33 rpm timecode runs 1.35x.
pub const SPEEDS: &[(&str, f64)] = &[("33 rpm", 1.0), ("45 rpm", 1.35)];

impl Default for Settings {
    fn default() -> Self {
        Self {
            accent: "Amber".to_string(),
            swap_input: false,
            brightness: 100,
            waveform_zoom: true,
            waveform_window: 16,
            timecode_enabled: true,
            input_device: "default".to_string(),
            output_device: "default".to_string(),
            online_updates: false,
            library: None,
            timecode: "serato_2a".to_string(),
            sample_rate: 48000,
            speed: 1.0,
            phono: false,
            buffer: 256,
            capture_buffer: 1024,
            capture_cpu: 3,
            playback_cpu: 2,
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

/// Places a music library is likely to live: mounted removable media, /mnt
/// and the user's Music folder. Only existing directories are returned.
pub fn library_candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut add_children = |dir: &str| {
        if let Ok(entries) = std::fs::read_dir(dir) {
            found.extend(entries.flatten().map(|e| e.path()));
        }
    };
    add_children("/mnt");
    // Drives mounted by the packaged udev rule: /media/<kernel name>
    add_children("/media");
    found.push(PathBuf::from("/media"));
    // /media/<user>/<volume> and /run/media/<user>/<volume>
    for base in ["/media", "/run/media"] {
        if let Ok(users) = std::fs::read_dir(base) {
            for user in users.flatten() {
                if let Ok(volumes) = std::fs::read_dir(user.path()) {
                    found.extend(volumes.flatten().map(|v| v.path()));
                }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        found.push(home.join("Music"));
    }
    found.retain(|p| p.is_dir());
    found.sort();
    found.dedup();
    found
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
            sample_rate: 96000,
            ..Settings::default()
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
