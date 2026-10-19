//! Looking up and downloading neowax releases from GitHub.
//!
//! Network access goes through `curl` (always present on the target system),
//! HTTPS only. Nothing here runs unless the user turned online updates on.

use crate::update::command_stdout;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "pascalhuerst/neowax";
/// Only assets under this prefix are downloaded, whatever the API says.
const DOWNLOAD_PREFIX: &str = "https://github.com/pascalhuerst/neowax/releases/download/";

/// A release package for this machine's architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineRelease {
    /// Debian version of the package, e.g. "0.1.7-1"
    pub version: String,
    pub file_name: String,
    pub url: String,
    /// Lower-case hex SHA-256 of the file, when GitHub reports one
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

/// Ask GitHub for the latest published release and pick the package for this
/// machine. `Ok(None)` means the release has no package for it.
pub fn latest_release() -> Result<Option<OnlineRelease>> {
    let arch = command_stdout("dpkg", &["--print-architecture"])?;
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--max-time", "15"])
        .args(["--header", "Accept: application/vnd.github+json"])
        .arg(&url)
        .output()
        .context("cannot run curl")?;
    if !out.status.success() {
        bail!("{}", curl_error(&out.stderr));
    }
    parse_release(&String::from_utf8_lossy(&out.stdout), &arch)
}

fn parse_release(json: &str, arch: &str) -> Result<Option<OnlineRelease>> {
    let release: Release = serde_json::from_str(json).context("unexpected reply from GitHub")?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    Ok(release.assets.into_iter().find_map(|asset| {
        let version = package_version(&asset.name, arch)?;
        if !asset.browser_download_url.starts_with(DOWNLOAD_PREFIX) {
            return None;
        }
        let sha256 = asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase);
        Some(OnlineRelease {
            version,
            file_name: asset.name,
            url: asset.browser_download_url,
            sha256,
        })
    }))
}

/// "neowax_0.1.7-1_arm64.deb" -> "0.1.7-1" for the given architecture.
/// Anything else, including odd characters in the version, gives `None`.
fn package_version(file_name: &str, arch: &str) -> Option<String> {
    let version = file_name
        .strip_prefix("neowax_")?
        .strip_suffix(&format!("_{arch}.deb"))?;
    let valid = !version.is_empty()
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+~-:".contains(c));
    valid.then(|| version.to_string())
}

/// Download `release` into `dir` and verify its checksum. Returns the file path.
pub fn download(release: &OnlineRelease, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {dir:?}"))?;
    let path = dir.join(&release.file_name);
    let out = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--max-time", "600"])
        .arg("--output")
        .arg(&path)
        .arg(&release.url)
        .output()
        .context("cannot run curl")?;
    if !out.status.success() {
        std::fs::remove_file(&path).ok();
        bail!("{}", curl_error(&out.stderr));
    }

    if let Some(expected) = &release.sha256 {
        let sums = command_stdout("sha256sum", &[&path.to_string_lossy()])?;
        let actual = sums.split_whitespace().next().unwrap_or_default();
        if !actual.eq_ignore_ascii_case(expected) {
            std::fs::remove_file(&path).ok();
            return Err(anyhow!("checksum mismatch, download discarded"));
        }
    } else {
        tracing::warn!(
            "GitHub gave no checksum for {}; relying on HTTPS",
            release.url
        );
    }
    Ok(path)
}

/// The last line of curl's error output, e.g. "curl: (6) Could not resolve host".
fn curl_error(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().trim_start_matches("curl: ").to_string())
        .unwrap_or_else(|| "request failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "tag_name": "v0.1.7", "draft": false, "prerelease": false,
        "assets": [
            {"name": "neowax-x86_64.AppImage",
             "browser_download_url": "https://github.com/pascalhuerst/neowax/releases/download/v0.1.7/neowax-x86_64.AppImage"},
            {"name": "neowax_0.1.7-1_arm64.deb",
             "browser_download_url": "https://github.com/pascalhuerst/neowax/releases/download/v0.1.7/neowax_0.1.7-1_arm64.deb",
             "digest": "sha256:ABCDEF0123"}
        ]
    }"#;

    #[test]
    fn picks_the_package_for_the_architecture() {
        let release = parse_release(SAMPLE, "arm64").unwrap().unwrap();
        assert_eq!(release.version, "0.1.7-1");
        assert_eq!(release.file_name, "neowax_0.1.7-1_arm64.deb");
        assert_eq!(release.sha256.as_deref(), Some("abcdef0123"));
        assert_eq!(parse_release(SAMPLE, "armhf").unwrap(), None);
    }

    #[test]
    fn ignores_drafts_prereleases_and_foreign_urls() {
        let draft = SAMPLE.replace("\"draft\": false", "\"draft\": true");
        assert_eq!(parse_release(&draft, "arm64").unwrap(), None);
        let pre = SAMPLE.replace("\"prerelease\": false", "\"prerelease\": true");
        assert_eq!(parse_release(&pre, "arm64").unwrap(), None);
        let foreign = SAMPLE.replace(
            "https://github.com/pascalhuerst/neowax/releases/download/v0.1.7/neowax_0.1.7-1_arm64.deb",
            "https://example.com/neowax_0.1.7-1_arm64.deb",
        );
        assert_eq!(parse_release(&foreign, "arm64").unwrap(), None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_release("not json", "arm64").is_err());
    }

    #[test]
    fn package_version_is_strict() {
        assert_eq!(
            package_version("neowax_1.2.3-1_arm64.deb", "arm64").as_deref(),
            Some("1.2.3-1")
        );
        assert_eq!(package_version("neowax_1.2.3-1_amd64.deb", "arm64"), None);
        assert_eq!(package_version("other_1.2.3-1_arm64.deb", "arm64"), None);
        assert_eq!(package_version("neowax_1;rm_arm64.deb", "arm64"), None);
        assert_eq!(package_version("neowax_../x_arm64.deb", "arm64"), None);
    }
}
