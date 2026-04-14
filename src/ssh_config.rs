use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parse an SSH config file and return a map of Host alias → HostName.
/// Skips wildcard hosts (containing `*` or `?`).
/// Returns an empty map on any I/O or parse error.
pub fn parse_ssh_config(path: &Path) -> HashMap<String, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let mut map = HashMap::new();
    let mut current_hosts: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Split on first whitespace or '='
        let (keyword, value) = match trimmed.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k, v.trim().trim_start_matches('=')),
            None => continue,
        };

        if keyword.eq_ignore_ascii_case("Host") {
            // New Host block — collect all aliases, skip wildcards
            current_hosts = value
                .split_whitespace()
                .filter(|h| !h.contains('*') && !h.contains('?'))
                .map(String::from)
                .collect();
        } else if keyword.eq_ignore_ascii_case("HostName") && !current_hosts.is_empty() {
            let hostname = value.trim().to_string();
            if !hostname.is_empty() {
                for alias in &current_hosts {
                    map.insert(alias.clone(), hostname.clone());
                }
            }
        } else if keyword.eq_ignore_ascii_case("Match") {
            // Match blocks are complex; clear current hosts to avoid misattribution
            current_hosts.clear();
        }
    }

    map
}

/// Resolve an SSH host to its real hostname using a specific config file.
/// Returns `None` if the file can't be read, the host isn't found, or
/// the resolved hostname equals the input (no-op).
pub fn resolve_ssh_hostname(host: &str, config_path: &Path) -> Option<String> {
    let map = parse_ssh_config(config_path);
    let resolved = map.get(host)?;
    if resolved == host {
        None
    } else {
        Some(resolved.clone())
    }
}

/// Resolve an SSH host using the default `~/.ssh/config`.
/// Returns `None` on any error or if the host is not found.
pub fn resolve_ssh_hostname_default(host: &str) -> Option<String> {
    let config_path = ssh_config_path()?;
    resolve_ssh_hostname(host, &config_path)
}

fn ssh_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("config"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_parse_basic_host_hostname() {
        let f = write_config("Host github-work\n  HostName github.com\n  User git\n");
        let map = parse_ssh_config(f.path());
        assert_eq!(map.get("github-work").unwrap(), "github.com");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_parse_multiple_host_blocks() {
        let f = write_config(
            "Host github-work\n  HostName github.com\n\n\
             Host gitlab-work\n  HostName gitlab.com\n",
        );
        let map = parse_ssh_config(f.path());
        assert_eq!(map.get("github-work").unwrap(), "github.com");
        assert_eq!(map.get("gitlab-work").unwrap(), "gitlab.com");
    }

    #[test]
    fn test_parse_multiple_aliases_per_host_line() {
        let f = write_config("Host gh ghub github-alias\n  HostName github.com\n");
        let map = parse_ssh_config(f.path());
        assert_eq!(map.get("gh").unwrap(), "github.com");
        assert_eq!(map.get("ghub").unwrap(), "github.com");
        assert_eq!(map.get("github-alias").unwrap(), "github.com");
    }

    #[test]
    fn test_parse_wildcard_hosts_skipped() {
        let f = write_config(
            "Host *\n  HostName default.com\n\n\
             Host *.example.com\n  HostName proxy.com\n\n\
             Host real-alias\n  HostName real.com\n",
        );
        let map = parse_ssh_config(f.path());
        assert!(!map.contains_key("*"));
        assert!(!map.contains_key("*.example.com"));
        assert_eq!(map.get("real-alias").unwrap(), "real.com");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_parse_case_insensitive_keywords() {
        let f = write_config("host my-server\n  hostname myserver.example.com\n");
        let map = parse_ssh_config(f.path());
        assert_eq!(map.get("my-server").unwrap(), "myserver.example.com");
    }

    #[test]
    fn test_parse_missing_file() {
        let map = parse_ssh_config(Path::new("/nonexistent/path/ssh_config"));
        assert!(map.is_empty());
    }

    #[test]
    fn test_parse_comments_and_empty_lines() {
        let f = write_config(
            "# This is a comment\n\n\
             Host my-host\n\
             # Another comment\n\
               HostName real-host.com\n\n",
        );
        let map = parse_ssh_config(f.path());
        assert_eq!(map.get("my-host").unwrap(), "real-host.com");
    }

    #[test]
    fn test_parse_equals_separator() {
        let f = write_config("Host=my-server\n  HostName=server.example.com\n");
        let map = parse_ssh_config(f.path());
        assert_eq!(map.get("my-server").unwrap(), "server.example.com");
    }

    #[test]
    fn test_parse_match_block_clears_hosts() {
        let f = write_config(
            "Host my-host\n  HostName real.com\n\n\
             Match host *.internal\n  HostName internal.com\n\n\
             Host another\n  HostName another.com\n",
        );
        let map = parse_ssh_config(f.path());
        assert_eq!(map.get("my-host").unwrap(), "real.com");
        assert_eq!(map.get("another").unwrap(), "another.com");
        // Match block should not produce entries
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn test_resolve_ssh_hostname_found() {
        let f = write_config("Host github-work\n  HostName github.com\n");
        assert_eq!(
            resolve_ssh_hostname("github-work", f.path()),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn test_resolve_ssh_hostname_not_found() {
        let f = write_config("Host github-work\n  HostName github.com\n");
        assert_eq!(resolve_ssh_hostname("other-host", f.path()), None);
    }

    #[test]
    fn test_resolve_ssh_hostname_same_as_input() {
        let f = write_config("Host github.com\n  HostName github.com\n");
        // No-op resolution returns None
        assert_eq!(resolve_ssh_hostname("github.com", f.path()), None);
    }

    #[test]
    fn test_resolve_dotted_alias() {
        let f = write_config("Host github.com\n  HostName internal-github.corp.example.com\n");
        assert_eq!(
            resolve_ssh_hostname("github.com", f.path()),
            Some("internal-github.corp.example.com".to_string())
        );
    }

    #[test]
    fn test_resolve_missing_config_file() {
        assert_eq!(
            resolve_ssh_hostname("anything", Path::new("/nonexistent")),
            None
        );
    }

    #[test]
    fn test_host_without_hostname_not_mapped() {
        let f = write_config("Host my-host\n  User git\n  Port 22\n");
        let map = parse_ssh_config(f.path());
        assert!(!map.contains_key("my-host"));
    }
}
