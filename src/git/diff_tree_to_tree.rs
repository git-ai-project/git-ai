use crate::error::GitAiError;
use crate::git::repository::{Repository, Tree, exec_git};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    TypeChange,
    Unmerged,
    Unknown,
}

impl DiffStatus {
    fn from_char(c: char) -> Self {
        match c {
            'A' => DiffStatus::Added,
            'D' => DiffStatus::Deleted,
            'M' => DiffStatus::Modified,
            'R' => DiffStatus::Renamed,
            'C' => DiffStatus::Copied,
            'T' => DiffStatus::TypeChange,
            'U' => DiffStatus::Unmerged,
            _ => DiffStatus::Unknown,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DiffFile {
    path: Option<PathBuf>,
    mode: String,
    oid: String,
}

impl DiffFile {
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[allow(dead_code)]
    pub fn mode(&self) -> &str {
        &self.mode
    }

    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.oid
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DiffDelta {
    status: DiffStatus,
    old_file: DiffFile,
    new_file: DiffFile,
    #[allow(dead_code)]
    similarity: u32,
}

impl DiffDelta {
    pub fn old_file(&self) -> &DiffFile {
        &self.old_file
    }

    pub fn new_file(&self) -> &DiffFile {
        &self.new_file
    }

    #[allow(dead_code)]
    pub fn status(&self) -> DiffStatus {
        self.status
    }

    #[allow(dead_code)]
    pub fn similarity(&self) -> u32 {
        self.similarity
    }
}

pub struct Diff {
    deltas: Vec<DiffDelta>,
}

impl Diff {
    pub fn deltas(&self) -> impl Iterator<Item = &DiffDelta> {
        self.deltas.iter()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.deltas.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }
}

impl Repository {
    /// Diff two trees, producing a Diff that describes the differences.
    /// This mimics git2::Repository::diff_tree_to_tree() using Git CLI.
    ///
    /// # Arguments
    /// * `old_tree` - The old tree to compare (None for empty tree)
    /// * `new_tree` - The new tree to compare (None for empty tree)
    /// * `_opts` - Diff options (currently unused, for API compatibility)
    /// * `pathspecs` - Optional set of paths to limit the diff to
    pub fn diff_tree_to_tree(
        &self,
        old_tree: Option<&Tree<'_>>,
        new_tree: Option<&Tree<'_>>,
        _opts: Option<()>,
        pathspecs: Option<&HashSet<String>>,
    ) -> Result<Diff, GitAiError> {
        // Get the empty tree OID if we need it
        let empty_tree_oid = if old_tree.is_none() || new_tree.is_none() {
            let mut args = self.global_args_for_exec();
            args.push("rev-parse".to_string());
            args.push("--empty-tree".to_string());
            let output = exec_git(&args)?;
            Some(String::from_utf8(output.stdout)?.trim().to_string())
        } else {
            None
        };

        // Determine the old and new tree OIDs
        let old_oid = if let Some(tree) = old_tree {
            tree.id()
        } else {
            empty_tree_oid.as_ref().unwrap().clone()
        };

        let new_oid = if let Some(tree) = new_tree {
            tree.id()
        } else {
            empty_tree_oid.as_ref().unwrap().clone()
        };

        // Use git diff to get the differences between trees
        // We use `git diff` instead of `git diff-tree` because it handles tree OIDs better
        // --raw: generate diff in raw format
        // -z: NUL-separated output
        // --no-abbrev: show full object names
        let mut args = self.global_args_for_exec();
        args.push("diff".to_string());
        args.push("--raw".to_string());
        args.push("-z".to_string());
        args.push("--no-abbrev".to_string());
        args.push(old_oid);
        args.push(new_oid);

        // Add pathspecs if provided
        if let Some(paths) = pathspecs {
            args.push("--".to_string());
            for path in paths {
                args.push(path.clone());
            }
        }

        let output = exec_git(&args)?;
        let deltas = parse_diff_raw(&output.stdout)?;

        Ok(Diff { deltas })
    }
}

/// Parse the raw output from git diff --raw -z
///
/// Format (when using -z, NUL bytes separate fields):
/// :<old_mode> <new_mode> <old_hash> <new_hash> <status>\0<path>\0
///
/// For renames/copies:
/// :<old_mode> <new_mode> <old_hash> <new_hash> R<score>\0<path>\0<old_path>\0
fn parse_diff_raw(data: &[u8]) -> Result<Vec<DiffDelta>, GitAiError> {
    let mut deltas = Vec::new();
    let mut parts = data
        .split(|byte| *byte == 0)
        .filter(|slice| !slice.is_empty())
        .peekable();

    while let Some(raw) = parts.next() {
        let metadata = std::str::from_utf8(raw)?;

        // Skip if the record doesn't start with ':' or is empty
        if !metadata.starts_with(':') || metadata.is_empty() {
            continue;
        }

        // When using -z, the path is the NEXT part after the NUL separator
        let path = match parts.next() {
            Some(p) => {
                let path_str = std::str::from_utf8(p)?;
                if path_str.is_empty() {
                    continue; // Skip records without a path
                }
                path_str
            }
            None => continue, // No path found
        };

        // Parse metadata: :<old_mode> <new_mode> <old_hash> <new_hash> <status>
        let mut fields = metadata[1..].split_whitespace(); // Skip the leading ':'
        let old_mode = match fields.next() {
            Some(m) => m,
            None => continue, // Skip if metadata is incomplete
        };
        let new_mode = match fields.next() {
            Some(m) => m,
            None => continue,
        };
        let old_hash = match fields.next() {
            Some(h) => h,
            None => continue,
        };
        let new_hash = match fields.next() {
            Some(h) => h,
            None => continue,
        };
        let status_str = match fields.next() {
            Some(s) => s,
            None => continue,
        };

        // Parse status (may include similarity score for R/C)
        let status_char = status_str.chars().next().unwrap_or('M');
        let status = DiffStatus::from_char(status_char);

        // Extract similarity score if present (e.g., "R95" -> 95)
        let similarity = if status_str.len() > 1 {
            status_str[1..].parse::<u32>().unwrap_or(0)
        } else {
            0
        };

        // For renames and copies, there are two paths
        let (new_path, old_path) = if matches!(status, DiffStatus::Renamed | DiffStatus::Copied) {
            let old_path_bytes = parts
                .next()
                .ok_or_else(|| GitAiError::Generic("Missing old path for rename/copy".into()))?;
            let old_path_str = std::str::from_utf8(old_path_bytes)?;
            (path.to_string(), Some(old_path_str.to_string()))
        } else {
            (path.to_string(), None)
        };

        // Construct the old_file and new_file
        let old_file = DiffFile {
            path: old_path
                .or_else(|| {
                    // For deletions, the old file path is the path
                    #[allow(clippy::if_same_then_else)]
                    if matches!(status, DiffStatus::Deleted) {
                        Some(new_path.clone())
                    } else {
                        Some(new_path.clone())
                    }
                })
                .map(PathBuf::from),
            mode: old_mode.to_string(),
            oid: old_hash.to_string(),
        };

        let new_file = DiffFile {
            path: Some(PathBuf::from(new_path.clone())),
            mode: new_mode.to_string(),
            oid: new_hash.to_string(),
        };

        deltas.push(DiffDelta {
            status,
            old_file,
            new_file,
            similarity,
        });
    }

    Ok(deltas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diff_raw() {
        // Sample output from git diff --raw -z (NUL-separated)
        let mut raw = Vec::new();

        // Modified file
        raw.extend_from_slice(b":100644 100644 5716ca5987cbf97d6bb54920bea6adde242d87e6 8f94139338f9404f26296befa88755fc2598c289 M\0src/lib.rs\0");

        // Added file
        raw.extend_from_slice(b":000000 100644 0000000000000000000000000000000000000000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 A\0src/new.rs\0");

        // Deleted file
        raw.extend_from_slice(b":100644 000000 1234567890abcdef1234567890abcdef12345678 0000000000000000000000000000000000000000 D\0src/old.rs\0");

        // Renamed file with 95% similarity
        raw.extend_from_slice(b":100644 100644 abcdef1234567890abcdef1234567890abcdef12 abcdef1234567890abcdef1234567890abcdef12 R95\0src/renamed.rs\0src/original.rs\0");

        let deltas = parse_diff_raw(&raw).expect("parse should succeed");

        assert_eq!(deltas.len(), 4);

        // Check modified file
        assert_eq!(deltas[0].status, DiffStatus::Modified);
        assert_eq!(deltas[0].new_file.path().unwrap(), Path::new("src/lib.rs"));

        // Check added file
        assert_eq!(deltas[1].status, DiffStatus::Added);
        assert_eq!(deltas[1].new_file.path().unwrap(), Path::new("src/new.rs"));

        // Check deleted file
        assert_eq!(deltas[2].status, DiffStatus::Deleted);
        assert_eq!(deltas[2].old_file.path().unwrap(), Path::new("src/old.rs"));

        // Check renamed file
        assert_eq!(deltas[3].status, DiffStatus::Renamed);
        assert_eq!(deltas[3].similarity, 95);
        assert_eq!(
            deltas[3].new_file.path().unwrap(),
            Path::new("src/renamed.rs")
        );
        assert_eq!(
            deltas[3].old_file.path().unwrap(),
            Path::new("src/original.rs")
        );
    }
}
