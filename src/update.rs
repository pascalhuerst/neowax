//! Self-update from a `.deb` file picked in the library.
//!
//! The package is inspected with `dpkg-deb`, compared against the installed
//! version with `dpkg`, and installed with `sudo -n dpkg -i`.

use anyhow::{anyhow, bail, Context, Result};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Name of the Debian package this binary ships in.
pub const PACKAGE_NAME: &str = "neowax";

/// Metadata read from a candidate `.deb` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebInfo {
    pub path: PathBuf,
    pub version: String,
    pub architecture: String,
}

pub fn is_deb(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("deb"))
}

/// Read the control fields of `path` and make sure it is a neowax package
/// that can run on this machine.
pub fn inspect(path: &Path) -> Result<DebInfo> {
    let out = Command::new("dpkg-deb")
        .arg("--field")
        .arg(path)
        .args(["Package", "Version", "Architecture"])
        .output()
        .context("cannot run dpkg-deb")?;
    if !out.status.success() {
        bail!("not a valid .deb file");
    }
    let info = parse_fields(path, &String::from_utf8_lossy(&out.stdout))?;

    let host_arch = command_stdout("dpkg", &["--print-architecture"])?;
    if info.architecture != "all" && info.architecture != host_arch {
        bail!(
            "wrong architecture {} (need {})",
            info.architecture,
            host_arch
        );
    }
    Ok(info)
}

fn parse_fields(path: &Path, fields: &str) -> Result<DebInfo> {
    let get = |key: &str| {
        fields
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .map(|v| v.trim().to_string())
            .ok_or_else(|| anyhow!("missing {key} field"))
    };
    let package = get("Package")?;
    if package != PACKAGE_NAME {
        bail!("not a {PACKAGE_NAME} package ({package})");
    }
    Ok(DebInfo {
        path: path.to_path_buf(),
        version: get("Version")?,
        architecture: get("Architecture")?,
    })
}

/// Version of the currently installed package, falling back to the version
/// this binary was built with (e.g. when run from a cargo build).
pub fn installed_version() -> String {
    command_stdout("dpkg-query", &["-W", "-f=${Version}", PACKAGE_NAME])
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// Compare `candidate` against `installed` using Debian version ordering.
pub fn compare_versions(candidate: &str, installed: &str) -> Result<Ordering> {
    let holds = |op: &str| -> Result<bool> {
        Ok(Command::new("dpkg")
            .args(["--compare-versions", candidate, op, installed])
            .status()
            .context("cannot run dpkg")?
            .success())
    };
    Ok(if holds("gt")? {
        Ordering::Greater
    } else if holds("lt")? {
        Ordering::Less
    } else {
        Ordering::Equal
    })
}

/// Install the package. Needs passwordless sudo (or running as root).
pub fn install(deb: &Path) -> Result<()> {
    let out = Command::new("sudo")
        .args(["-n", "dpkg", "-i"])
        .arg(deb)
        .output()
        .context("cannot run sudo")?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let reason = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("dpkg failed");
    Err(anyhow!("{}", reason.trim()))
}

/// Verify that the installed binary still accepts the arguments this process
/// was started with, so a restart can't leave the player dead.
pub fn check_restart_args() -> Result<()> {
    let exe = restart_path()?;
    let out = Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .arg("--check-args")
        .output()
        .with_context(|| format!("cannot run {}", exe.display()))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let reason = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("error:"))
        .unwrap_or("new version rejects the current arguments");
    bail!("installed, but not restarting: {}", reason.trim())
}

/// Path to re-launch after an update. The running binary was replaced by
/// dpkg, so `/proc/self/exe` carries a " (deleted)" suffix.
pub fn restart_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let s = exe.to_string_lossy();
    Ok(PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s)))
}

fn command_stdout(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("cannot run {cmd}"))?;
    if !out.status.success() {
        bail!("{cmd} failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_neowax_fields() {
        let info = parse_fields(
            Path::new("x.deb"),
            "Package: neowax\nVersion: 0.2.0-1\nArchitecture: arm64\n",
        )
        .unwrap();
        assert_eq!(info.version, "0.2.0-1");
        assert_eq!(info.architecture, "arm64");
    }

    #[test]
    fn rejects_other_packages() {
        let err = parse_fields(
            Path::new("x.deb"),
            "Package: vim\nVersion: 1\nArchitecture: arm64\n",
        );
        assert!(err.is_err());
    }

    #[test]
    fn detects_deb_extension() {
        assert!(is_deb(Path::new("/a/b/neowax_0.2.0_arm64.DEB")));
        assert!(!is_deb(Path::new("/a/b/song.mp3")));
    }
}
