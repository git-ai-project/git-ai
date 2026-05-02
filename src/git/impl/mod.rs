/// Fast git operations that parse .git internals directly
///
/// These implementations avoid subprocess spawns for common read-only operations.
/// They handle the "easy 80%" of cases (loose refs, packed-refs, loose objects)
/// and fallback to git CLI for complex cases (packfiles, delta compression, etc.)
///
/// Safety: Only reads .git directory, never writes. All functions are read-only.

pub mod refs;
pub mod objects;
pub mod config;

pub use refs::FastRefReader;
pub use objects::FastObjectReader;
pub use config::FastConfigReader;

use crate::error::GitAiError;
use std::path::PathBuf;

/// Fast git reader that can handle common operations without subprocess spawns
pub struct FastGitReader {
    pub git_dir: PathBuf,
}

impl FastGitReader {
    /// Create a new fast reader for the given git directory
    pub fn new(git_dir: PathBuf) -> Self {
        Self { git_dir }
    }

    /// Try to resolve a ref to a SHA, returns None if we need to fallback to git CLI
    ///
    /// Handles:
    /// - Loose refs (refs/heads/main, refs/remotes/origin/main)
    /// - Packed refs (after git gc)
    /// - HEAD (symbolic or detached)
    ///
    /// Does NOT handle:
    /// - Complex rev-parse syntax (HEAD~3, @{yesterday})
    /// - Worktree refs
    /// - Symbolic ref chains beyond HEAD
    pub fn try_resolve_ref(&self, refname: &str) -> Result<Option<String>, GitAiError> {
        let ref_reader = FastRefReader::new(&self.git_dir);
        ref_reader.resolve_ref(refname)
    }

    /// Read HEAD symbolic ref, returns None if we need to fallback
    ///
    /// Handles:
    /// - .git/HEAD containing "ref: refs/heads/main"
    /// - .git/HEAD containing a direct SHA (detached HEAD)
    ///
    /// Does NOT handle:
    /// - Worktree HEAD files
    pub fn try_read_head_symbolic(&self) -> Result<Option<String>, GitAiError> {
        let ref_reader = FastRefReader::new(&self.git_dir);
        ref_reader.read_head_symbolic()
    }

    /// Read a blob object, returns None if we need to fallback
    ///
    /// Handles:
    /// - Loose objects (.git/objects/ab/cdef123...)
    ///
    /// Does NOT handle:
    /// - Packed objects (requires pack file parsing)
    /// - Delta-compressed objects
    pub fn try_read_blob(&self, sha: &str) -> Result<Option<Vec<u8>>, GitAiError> {
        let obj_reader = FastObjectReader::new(&self.git_dir);
        obj_reader.read_loose_blob(sha)
    }

    /// Read a config value from .git/config, returns None if not found
    ///
    /// Handles:
    /// - Simple key=value in .git/config
    ///
    /// Does NOT handle:
    /// - Global config (~/.gitconfig)
    /// - System config (/etc/gitconfig)
    /// - Complex multi-line values
    pub fn try_read_config(&self, section: &str, key: &str) -> Result<Option<String>, GitAiError> {
        let config_reader = FastConfigReader::new(&self.git_dir);
        config_reader.read_value(section, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fast_git_reader_creates() {
        let reader = FastGitReader::new(PathBuf::from("/tmp/test/.git"));
        assert_eq!(reader.git_dir, PathBuf::from("/tmp/test/.git"));
    }
}
