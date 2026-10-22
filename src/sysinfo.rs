//! Read-only facts about the system neowax runs on.

/// Human readable OS name from `/etc/os-release` (e.g. "Raspbian GNU/Linux 12 (bookworm)").
pub fn os_version() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| parse_pretty_name(&s))
        .unwrap_or_else(|| "unknown".to_string())
}

/// CPU cores reserved with the `isolcpus=` kernel option. The scheduler keeps
/// ordinary threads off them, which makes them the right place for the audio
/// threads. Empty if none are isolated or the information is unavailable.
pub fn isolated_cpus() -> Vec<usize> {
    std::fs::read_to_string("/sys/devices/system/cpu/isolated")
        .map(|s| parse_cpu_list(&s))
        .unwrap_or_default()
}

/// Parse a kernel CPU list such as "2-3,5" into core numbers. Anything that
/// isn't a number or a range is skipped.
fn parse_cpu_list(list: &str) -> Vec<usize> {
    let mut cpus = Vec::new();
    for part in list.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    cpus.extend(a..=b);
                }
            }
            None => cpus.extend(part.trim().parse::<usize>()),
        }
    }
    cpus
}

/// Format core numbers compactly, e.g. [2, 3, 5] -> "2-3,5". Empty gives "none".
pub fn format_cpu_list(cpus: &[usize]) -> String {
    let mut sorted = cpus.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
        i += 1;
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(",")
    }
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
    fn parses_cpu_lists() {
        assert_eq!(parse_cpu_list("2-3\n"), vec![2, 3]);
        assert_eq!(parse_cpu_list("0,2-4,7"), vec![0, 2, 3, 4, 7]);
        assert_eq!(parse_cpu_list("\n"), Vec::<usize>::new());
        assert_eq!(parse_cpu_list(""), Vec::<usize>::new());
        assert_eq!(parse_cpu_list("x,3,a-b"), vec![3]);
    }

    #[test]
    fn formats_cpu_lists_compactly() {
        assert_eq!(format_cpu_list(&[2, 3]), "2-3");
        assert_eq!(format_cpu_list(&[5, 2, 3, 3]), "2-3,5");
        assert_eq!(format_cpu_list(&[1]), "1");
        assert_eq!(format_cpu_list(&[]), "none");
    }

    #[test]
    fn missing_pretty_name() {
        assert_eq!(parse_pretty_name("NAME=x\n"), None);
    }
}
