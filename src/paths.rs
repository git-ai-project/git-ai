use std::path::PathBuf;

/// Get the user's home directory, or None if unavailable.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

/// Get the user's home directory with a platform-appropriate fallback.
/// Daemon code that must not fail uses this variant.
pub fn home_dir_or_tmp() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
    }
    #[cfg(windows)]
    {
        PathBuf::from(
            std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("APPDATA"))
                .unwrap_or_else(|_| "C:\\Temp".to_string()),
        )
    }
}

/// The root config/data directory: `~/.git-ai`
pub fn git_ai_dir() -> PathBuf {
    home_dir_or_tmp().join(".git-ai")
}

/// The internal directory: `~/.git-ai/internal`
pub fn git_ai_internal_dir() -> PathBuf {
    git_ai_dir().join("internal")
}

/// The daemon directory: `~/.git-ai/internal/daemon`
pub fn daemon_dir() -> PathBuf {
    git_ai_internal_dir().join("daemon")
}

/// Write a file with owner-only permissions (0600 on unix).
pub fn write_private(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(content)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
    }
}

/// Encode bytes as lowercase hex string.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}
