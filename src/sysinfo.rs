//! Read-only facts about the system neowax runs on.

/// Human readable OS name from `/etc/os-release` (e.g. "Raspbian GNU/Linux 12 (bookworm)").
pub fn os_version() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| parse_pretty_name(&s))
        .unwrap_or_else(|| "unknown".to_string())
}

fn parse_pretty_name(os_release: &str) -> Option<String> {
    os_release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_pretty_name() {
        let text = "NAME=\"Debian\"\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\n";
        assert_eq!(
            parse_pretty_name(text).as_deref(),
            Some("Debian GNU/Linux 12 (bookworm)")
        );
    }

    #[test]
    fn missing_pretty_name() {
        assert_eq!(parse_pretty_name("NAME=x\n"), None);
    }
}
