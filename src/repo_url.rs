use url::Url;

/// Normalize repo URL to canonical HTTPS format
/// Accepts: HTTPS, HTTP, SSH (scp-like user@host:path or ssh://), git:// URLs
/// Returns: Canonical HTTPS URL without credentials, .git suffix, or trailing slash
pub fn normalize_repo_url(url_str: &str) -> Result<String, String> {
    let url_str = url_str.trim();

    // Handle SSH scp-like format: user@host:path
    if !url_str.contains("://") {
        if let Some((user_host, path)) = url_str.split_once(':') {
            if let Some((_, host)) = user_host.rsplit_once('@') {
                return normalize_ssh_url(host, path);
            }
        }
    }

    // Parse as URL
    let url = Url::parse(url_str).map_err(|e| format!("Invalid URL: {}", e))?;

    // Validate scheme
    let scheme = url.scheme();
    if !["https", "http", "git", "ssh"].contains(&scheme) {
        return Err(format!("Unsupported URL scheme: {}", scheme));
    }

    // Extract host
    let host = url.host_str().ok_or("URL must have a host")?;

    // Normalize path: remove .git suffix and trailing slash
    let path = url
        .path()
        .trim_end_matches('/')
        .trim_end_matches(".git");

    // Build canonical HTTPS URL
    let canonical = format!("https://{}{}", host, path);

    // Validate the normalized URL
    validate_normalized_url(&canonical)?;

    Ok(canonical)
}

/// Validate that normalized URL is a proper HTTPS URL
fn validate_normalized_url(url_str: &str) -> Result<(), String> {
    let url = Url::parse(url_str).map_err(|e| format!("Failed to parse normalized URL: {}", e))?;

    if url.scheme() != "https" {
        return Err("Normalized URL must be HTTPS".to_string());
    }

    if url.host_str().is_none() {
        return Err("Normalized URL must have a valid host".to_string());
    }

    // Ensure path is not empty (at minimum /)
    if url.path().is_empty() || url.path() == "/" {
        return Err("Normalized URL must have a path (repo identifier)".to_string());
    }

    Ok(())
}

/// Normalize SSH scp-like URL (user@host:path) to HTTPS
fn normalize_ssh_url(host: &str, path: &str) -> Result<String, String> {
    if host.is_empty() || path.is_empty() {
        return Err("Invalid SSH URL format".to_string());
    }

    // Normalize path
    let path = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .trim_end_matches(".git");

    let canonical = format!("https://{}/{}", host, path);

    // Validate the normalized URL
    validate_normalized_url(&canonical)?;

    Ok(canonical)
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
        assert!(normalize_repo_url("git@github.com").is_err()); // missing :path
    }
}
