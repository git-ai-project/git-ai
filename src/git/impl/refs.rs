/// Fast ref resolution without subprocess spawns
///
/// Handles loose refs and packed-refs parsing.

use crate::error::GitAiError;
use std::fs;
use std::path::{Path, PathBuf};

pub struct FastRefReader {
    git_dir: PathBuf,
    packed_refs_cache: Option<String>,
}

impl FastRefReader {
    pub fn new(git_dir: &Path) -> Self {
        Self {
            git_dir: git_dir.to_path_buf(),
            packed_refs_cache: None,
        }
    }

    /// Read HEAD and return the symbolic ref it points to
    ///
    /// Returns:
    /// - Some("refs/heads/main") if HEAD contains "ref: refs/heads/main"
    /// - Some("<sha>") if HEAD is detached (direct SHA)
    /// - None if we can't read HEAD (fallback to git CLI)
    pub fn read_head_symbolic(&self) -> Result<Option<String>, GitAiError> {
        let head_path = self.git_dir.join("HEAD");

        let content = match fs::read_to_string(&head_path) {
            Ok(c) => c,
            Err(_) => return Ok(None), // Can't read, fallback
        };

        let trimmed = content.trim();

        // Check for symbolic ref: "ref: refs/heads/main"
        if let Some(refname) = trimmed.strip_prefix("ref: ") {
            Ok(Some(refname.to_string()))
        } else if trimmed.len() == 40 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            // Detached HEAD - direct SHA
            Ok(Some(trimmed.to_string()))
        } else {
            // Unexpected format, fallback
            Ok(None)
        }
    }

    /// Resolve a refname to a SHA
    ///
    /// Tries in order:
    /// 1. Loose ref (.git/refs/heads/main)
    /// 2. Packed ref (.git/packed-refs)
    /// 3. Returns None (fallback to git CLI)
    pub fn resolve_ref(&self, refname: &str) -> Result<Option<String>, GitAiError> {
        // Try loose ref first
        if let Some(sha) = self.try_loose_ref(refname)? {
            return Ok(Some(sha));
        }

        // Try packed-refs
        if let Some(sha) = self.try_packed_ref(refname)? {
            return Ok(Some(sha));
        }

        // Not found
        Ok(None)
    }

    /// Try to read a loose ref file
    fn try_loose_ref(&self, refname: &str) -> Result<Option<String>, GitAiError> {
        // Handle HEAD specially
        if refname == "HEAD" {
            return self.read_head_symbolic();
        }

        let ref_path = self.git_dir.join(refname);

        match fs::read_to_string(&ref_path) {
            Ok(content) => {
                let sha = content.trim();
                // Validate it looks like a SHA
                if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    Ok(Some(sha.to_string()))
                } else if sha.starts_with("ref: ") {
                    // Symbolic ref, resolve it recursively (one level only to avoid loops)
                    let target = sha.strip_prefix("ref: ").unwrap();
                    self.try_loose_ref(target)
                } else {
                    // Malformed, fallback
                    Ok(None)
                }
            }
            Err(_) => Ok(None), // File doesn't exist or can't be read
        }
    }

    /// Try to find a ref in packed-refs
    fn try_packed_ref(&self, refname: &str) -> Result<Option<String>, GitAiError> {
        let packed_refs_path = self.git_dir.join("packed-refs");

        // Read packed-refs file
        let content = match fs::read_to_string(&packed_refs_path) {
            Ok(c) => c,
            Err(_) => return Ok(None), // No packed-refs file
        };

        // Parse packed-refs format:
        // # pack-refs with: peeled fully-peeled sorted
        // abc123... refs/heads/main
        // def456... refs/heads/feature
        // ^789abc... (peeled tag annotation)

        for line in content.lines() {
            // Skip comments
            if line.starts_with('#') {
                continue;
            }

            // Skip peeled annotations (start with ^)
            if line.starts_with('^') {
                continue;
            }

            // Parse: <sha> <refname>
            let mut parts = line.split_whitespace();
            let sha = match parts.next() {
                Some(s) => s,
                None => continue,
            };
            let name = match parts.next() {
                Some(n) => n,
                None => continue,
            };

            if name == refname {
                // Validate SHA format
                if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Ok(Some(sha.to_string()));
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_git_dir() -> TempDir {
        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
        fs::create_dir_all(git_dir.join("refs/remotes/origin")).unwrap();
        temp
    }

    #[test]
    fn test_read_head_symbolic_ref() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        // Write HEAD with symbolic ref
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let reader = FastRefReader::new(&git_dir);
        let result = reader.read_head_symbolic().unwrap();

        assert_eq!(result, Some("refs/heads/main".to_string()));
    }

    #[test]
    fn test_read_head_detached() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        // Write HEAD with direct SHA
        let sha = "abc123def456789012345678901234567890abcd";
        fs::write(git_dir.join("HEAD"), format!("{}\n", sha)).unwrap();

        let reader = FastRefReader::new(&git_dir);
        let result = reader.read_head_symbolic().unwrap();

        assert_eq!(result, Some(sha.to_string()));
    }

    #[test]
    fn test_resolve_loose_ref() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let sha = "abc123def456789012345678901234567890abcd";
        fs::write(
            git_dir.join("refs/heads/main"),
            format!("{}\n", sha),
        )
        .unwrap();

        let reader = FastRefReader::new(&git_dir);
        let result = reader.resolve_ref("refs/heads/main").unwrap();

        assert_eq!(result, Some(sha.to_string()));
    }

    #[test]
    fn test_resolve_packed_ref() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let sha1 = "abc123def456789012345678901234567890abcd";
        let sha2 = "def456abc789012345678901234567890abcd123";

        // Write packed-refs
        let packed_refs = format!(
            "# pack-refs with: peeled fully-peeled sorted\n\
             {} refs/heads/main\n\
             {} refs/heads/feature\n",
            sha1, sha2
        );
        fs::write(git_dir.join("packed-refs"), packed_refs).unwrap();

        let reader = FastRefReader::new(&git_dir);

        let result1 = reader.resolve_ref("refs/heads/main").unwrap();
        assert_eq!(result1, Some(sha1.to_string()));

        let result2 = reader.resolve_ref("refs/heads/feature").unwrap();
        assert_eq!(result2, Some(sha2.to_string()));
    }

    #[test]
    fn test_loose_ref_overrides_packed() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let loose_sha = "abc123def456789012345678901234567890abcd";
        let packed_sha = "def456abc789012345678901234567890abcd123";

        // Write both loose and packed
        fs::write(
            git_dir.join("refs/heads/main"),
            format!("{}\n", loose_sha),
        )
        .unwrap();

        let packed_refs = format!(
            "# pack-refs with: peeled fully-peeled sorted\n\
             {} refs/heads/main\n",
            packed_sha
        );
        fs::write(git_dir.join("packed-refs"), packed_refs).unwrap();

        let reader = FastRefReader::new(&git_dir);
        let result = reader.resolve_ref("refs/heads/main").unwrap();

        // Loose ref should win
        assert_eq!(result, Some(loose_sha.to_string()));
    }

    #[test]
    fn test_nonexistent_ref_returns_none() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let reader = FastRefReader::new(&git_dir);
        let result = reader.resolve_ref("refs/heads/nonexistent").unwrap();

        assert_eq!(result, None);
    }
}
