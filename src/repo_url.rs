/// Normalize repo URL to canonical HTTPS format
/// Accepts: HTTPS, HTTP, SSH (scp-like user@host:path or ssh://), git:// URLs
/// Returns: Canonical HTTPS URL without credentials, .git suffix, or trailing slash
pub fn normalize_repo_url(url_str: &str) -> Result<String, String> {
    let url_str = url_str.trim();

    if url_str.is_empty() {
        return Err("Empty URL".to_string());
    }

    // Handle SSH scp-like format: user@host:path
    if !url_str.contains("://")
        && let Some((user_host, path)) = url_str.split_once(':')
        && let Some((_, host)) = user_host.rsplit_once('@')
    {
        return normalize_ssh_url(host, path);
    }

    // Parse as URL
    let (scheme, rest) = url_str
        .split_once("://")
        .ok_or_else(|| format!("Invalid URL: {}", url_str))?;

    if !["https", "http", "git", "ssh"].contains(&scheme) {
        return Err(format!("Unsupported URL scheme: {}", scheme));
    }

    // Split authority from path
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };

    // Strip userinfo
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };

    // Strip port
    let host = match host_port.rfind(':') {
        Some(i) if host_port[i + 1..].parse::<u16>().is_ok() => &host_port[..i],
        _ => host_port,
    };

    if host.is_empty() {
        return Err("URL must have a host".to_string());
    }

    // Normalize path: remove .git suffix and trailing slash
    let path = path.trim_end_matches('/').trim_end_matches(".git");

    if path.is_empty() || path == "/" {
        return Err("Normalized URL must have a path (repo identifier)".to_string());
    }

    let canonical = format!("https://{}{}", host, path);
    validate_normalized_url(&canonical)?;
    Ok(canonical)
}

fn validate_normalized_url(url_str: &str) -> Result<(), String> {
    if !url_str.starts_with("https://") {
        return Err("Normalized URL must be HTTPS".to_string());
    }
    let rest = &url_str["https://".len()..];
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if host.is_empty() {
        return Err("Normalized URL must have a valid host".to_string());
    }
    if path.is_empty() || path == "/" {
        return Err("Normalized URL must have a path (repo identifier)".to_string());
    }
    Ok(())
}

fn normalize_ssh_url(host: &str, path: &str) -> Result<String, String> {
    if host.is_empty() || path.is_empty() {
        return Err("Invalid SSH URL format".to_string());
    }

    let path = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .trim_end_matches(".git");

    let canonical = format!("https://{}/{}", host, path);
    validate_normalized_url(&canonical)?;
    Ok(canonical)
}

pub fn resolve_repo_url_from_repo(repo: &crate::git::repository::Repository) -> Option<String> {
    let remote_name = repo.get_default_remote().ok()??;
    let remotes = repo.remotes_with_urls().ok()?;
    let (_, url) = remotes.into_iter().find(|(n, _)| n == &remote_name)?;
    normalize_repo_url(&url).ok()
}

pub fn resolve_repo_url_from_path(work_dir: &std::path::Path) -> Option<String> {
    let repo = crate::git::repository::discover_repository_in_path_no_git_exec(work_dir).ok()?;
    resolve_repo_url_from_repo(&repo)
}

#[cfg(test)]
mod tests {
    use super::normalize_repo_url;

    #[test]
    fn test_normalize_repo_url_https() {
        assert_eq!(
            normalize_repo_url("https://github.com/user/repo").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("https://github.com/user/repo.git").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("https://github.com/user/repo/").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("https://gitlab.com/group/subgroup/repo.git/").unwrap(),
            "https://gitlab.com/group/subgroup/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_ssh() {
        assert_eq!(
            normalize_repo_url("git@github.com:user/repo.git").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("ssh://git@github.com/user/repo.git").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("alice@github.com:org/repo").unwrap(),
            "https://github.com/org/repo"
        );
        assert_eq!(
            normalize_repo_url("git@gitlab.com:group/subgroup/repo").unwrap(),
            "https://gitlab.com/group/subgroup/repo"
        );
        assert_eq!(
            normalize_repo_url("git@bitbucket.org:user/repo.git").unwrap(),
            "https://bitbucket.org/user/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_git_protocol() {
        assert_eq!(
            normalize_repo_url("git://github.com/user/repo.git").unwrap(),
            "https://github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_http_upgrade() {
        assert_eq!(
            normalize_repo_url("http://github.com/user/repo").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("https://token@github.com/user/repo").unwrap(),
            "https://github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_invalid() {
        assert!(normalize_repo_url("not-a-url").is_err());
        assert!(normalize_repo_url("https://").is_err());
        assert!(normalize_repo_url("ftp://example.com/repo").is_err());
        assert!(normalize_repo_url("git@github.com").is_err());
    }

    #[test]
    fn test_normalize_repo_url_ssh_scp_edge_cases() {
        assert_eq!(
            normalize_repo_url("git@github.com:/user/repo").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("git@gitlab.example.com:group/subgroup/nested/repo").unwrap(),
            "https://gitlab.example.com/group/subgroup/nested/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_empty_or_invalid_ssh() {
        let result = normalize_repo_url("git@github.com:");
        assert!(result.is_err());
        let result = normalize_repo_url("");
        assert!(result.is_err());
        let result = normalize_repo_url("   ");
        assert!(result.is_err());
    }

    #[test]
    fn test_normalize_repo_url_with_credentials() {
        assert_eq!(
            normalize_repo_url("https://user:pass@github.com/user/repo").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("https://oauth2:token123@gitlab.com/user/repo").unwrap(),
            "https://gitlab.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_with_port() {
        assert_eq!(
            normalize_repo_url("https://github.com:443/user/repo").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("ssh://git@github.com:22/user/repo.git").unwrap(),
            "https://github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_no_path() {
        let result = normalize_repo_url("https://github.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("path"));
        let result = normalize_repo_url("https://github.com/");
        assert!(result.is_err());
    }

    #[test]
    fn test_normalize_repo_url_complex_paths() {
        assert_eq!(
            normalize_repo_url("https://github.com/user/repo.git.git").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("https://github.com/my-org/my_repo-123").unwrap(),
            "https://github.com/my-org/my_repo-123"
        );
        assert_eq!(
            normalize_repo_url("https://github.com/user/repo.v2").unwrap(),
            "https://github.com/user/repo.v2"
        );
        assert_eq!(
            normalize_repo_url("https://gitlab.com/group/subgroup/project.git").unwrap(),
            "https://gitlab.com/group/subgroup/project"
        );
    }

    #[test]
    fn test_validate_normalized_url() {
        use super::validate_normalized_url;
        assert!(validate_normalized_url("https://github.com/user/repo").is_ok());
        assert!(validate_normalized_url("http://github.com/user/repo").is_err());
        assert!(validate_normalized_url("https://github.com").is_err());
        assert!(validate_normalized_url("https://github.com/").is_err());
    }

    #[test]
    fn test_normalize_ssh_url_edge_cases() {
        use super::normalize_ssh_url;
        assert_eq!(
            normalize_ssh_url("github.com", "user/repo/").unwrap(),
            "https://github.com/user/repo"
        );
        assert!(normalize_ssh_url("", "user/repo").is_err());
        assert!(normalize_ssh_url("github.com", "").is_err());
        assert_eq!(
            normalize_ssh_url("gitlab.com", "group/repo.git").unwrap(),
            "https://gitlab.com/group/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_whitespace_handling() {
        assert_eq!(
            normalize_repo_url("  https://github.com/user/repo  ").unwrap(),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_repo_url("  git@github.com:user/repo.git  ").unwrap(),
            "https://github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_unsupported_schemes() {
        assert!(normalize_repo_url("ftp://example.com/repo").is_err());
        assert!(normalize_repo_url("file:///local/path").is_err());
        assert!(normalize_repo_url("svn://example.com/repo").is_err());
    }
}
