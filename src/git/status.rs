use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git};
use std::collections::HashSet;
use std::str;

/// Maximum number of pathspec arguments to pass on the command line.
/// Beyond this threshold, we run git without pathspecs and post-filter
/// in Rust to avoid OS `ARG_MAX` / E2BIG errors.
pub const MAX_PATHSPEC_ARGS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
    Ignored,
    Unknown(char),
}

impl From<char> for StatusCode {
    fn from(value: char) -> Self {
        match value {
            '.' => StatusCode::Unmodified,
            'M' => StatusCode::Modified,
            'A' => StatusCode::Added,
            'D' => StatusCode::Deleted,
            'R' => StatusCode::Renamed,
            'C' => StatusCode::Copied,
            'U' => StatusCode::Unmerged,
            '?' => StatusCode::Untracked,
            '!' => StatusCode::Ignored,
            other => StatusCode::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Ordinary,
    Rename,
    Copy,
    Unmerged,
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub staged: StatusCode,
    pub unstaged: StatusCode,
    pub kind: EntryKind,
    pub orig_path: Option<String>,
}

impl Repository {
    // Get status for tracked files that changed
    pub fn get_staged_filenames(&self) -> Result<HashSet<String>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("diff".to_string());
        args.push("--cached".to_string());
        args.push("--name-only".to_string());
        args.push("-z".to_string()); // NUL-separated output for proper UTF-8 handling

        let output = exec_git(&args)?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git diff exited with status {}",
                output.status
            )));
        }

        // With -z, output is NUL-separated. The output may contain a trailing NUL.
        let filenames: HashSet<String> = output
            .stdout
            .split(|&b| b == 0)
            .filter(|bytes| !bytes.is_empty())
            .filter_map(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .collect();

        Ok(filenames)
    }

    // Get status for tracked files that changed
    pub fn get_staged_and_unstaged_filenames(&self) -> Result<HashSet<String>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("status".to_string());
        args.push("--porcelain=v2".to_string());
        args.push("-z".to_string());

        let output = exec_git(&args)?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git status exited with status {}",
                output.status
            )));
        }

        let entries = parse_porcelain_v2(&output.stdout)?;

        let filenames: HashSet<String> = entries
            .iter()
            .filter(|entry| entry.kind != EntryKind::Ignored)
            .map(|entry| entry.path.clone())
            .collect();

        Ok(filenames)
    }

    pub fn status(
        &self,
        pathspecs: Option<&HashSet<String>>,
        skip_untracked: bool,
    ) -> Result<Vec<StatusEntry>, GitAiError> {
        let staged_filenames = self.get_staged_filenames()?;

        let combined_pathspecs: HashSet<String> = if let Some(paths) = pathspecs {
            staged_filenames.union(paths).cloned().collect()
        } else {
            staged_filenames
        };

        // When no explicit pathspecs are provided and nothing is staged,
        // we still need a full status scan to capture unstaged changes.
        let should_full_scan = pathspecs.is_none() && combined_pathspecs.is_empty();
        if combined_pathspecs.is_empty() && !should_full_scan {
            return Ok(Vec::new());
        }

        let mut args = self.global_args_for_exec();
        args.push("status".to_string());
        args.push("--porcelain=v2".to_string());
        args.push("-z".to_string());

        if skip_untracked {
            args.push("--untracked-files=no".to_string());
        }

        // Add combined pathspecs as CLI args only if under the threshold;
        // otherwise run without pathspecs and post-filter to avoid E2BIG.
        let needs_post_filter = !should_full_scan && combined_pathspecs.len() > MAX_PATHSPEC_ARGS;
        if !should_full_scan && !needs_post_filter && !combined_pathspecs.is_empty() {
            args.push("--".to_string());
            for path in &combined_pathspecs {
                args.push(path.clone());
            }
        }

        let output = exec_git(&args)?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git status exited with status {}",
                output.status
            )));
        }

        let mut entries = parse_porcelain_v2(&output.stdout)?;

        if needs_post_filter {
            entries.retain(|e| {
                combined_pathspecs.contains(&e.path)
                    || e.orig_path
                        .as_ref()
                        .is_some_and(|op| combined_pathspecs.contains(op))
            });
        }

        Ok(entries)
    }
}

fn parse_porcelain_v2(data: &[u8]) -> Result<Vec<StatusEntry>, GitAiError> {
    let mut entries = Vec::new();
    let mut parts = data
        .split(|byte| *byte == 0)
        .filter(|slice| !slice.is_empty())
        .peekable();

    while let Some(raw) = parts.next() {
        let record = str::from_utf8(raw)?;
        let mut chars = record.chars();
        let tag = chars
            .next()
            .ok_or_else(|| GitAiError::Generic("Unexpected empty porcelain v2 record".into()))?;

        match tag {
            '1' | 'u' => {
                let mut fields = record.splitn(9, ' ');
                let _ = fields.next(); // tag
                let xy = fields
                    .next()
                    .ok_or_else(|| GitAiError::Generic("Missing XY field".into()))?;
                if xy.len() != 2 {
                    return Err(GitAiError::Generic(format!(
                        "Unexpected XY field length: {}",
                        xy
                    )));
                }
                let staged = StatusCode::from(xy.chars().next().unwrap());
                let unstaged = StatusCode::from(xy.chars().nth(1).unwrap());

                // skip submodule/metadata fields to capture path
                for _ in 0..6 {
                    fields.next();
                }

                let path = fields
                    .next()
                    .ok_or_else(|| GitAiError::Generic("Missing path field".into()))?
                    .to_string();

                entries.push(StatusEntry {
                    path,
                    staged,
                    unstaged,
                    kind: if matches!(staged, StatusCode::Unmerged)
                        || matches!(unstaged, StatusCode::Unmerged)
                    {
                        EntryKind::Unmerged
                    } else {
                        EntryKind::Ordinary
                    },
                    orig_path: None,
                });
            }
            '2' => {
                let mut fields = record.splitn(10, ' ');
                let _ = fields.next(); // tag
                let xy = fields
                    .next()
                    .ok_or_else(|| GitAiError::Generic("Missing XY field".into()))?;
                if xy.len() != 2 {
                    return Err(GitAiError::Generic(format!(
                        "Unexpected XY field length: {}",
                        xy
                    )));
                }
                let staged = StatusCode::from(xy.chars().next().unwrap());
                let unstaged = StatusCode::from(xy.chars().nth(1).unwrap());

                // skip submodule/metadata fields
                for _ in 0..7 {
                    fields.next();
                }

                let path = fields
                    .next()
                    .ok_or_else(|| GitAiError::Generic("Missing path field".into()))?
                    .to_string();

                let orig_path_bytes = parts.next().ok_or_else(|| {
                    GitAiError::Generic("Missing original path for rename/copy".into())
                })?;
                let orig_path = str::from_utf8(orig_path_bytes)?.to_string();

                let kind = match staged {
                    StatusCode::Renamed => EntryKind::Rename,
                    StatusCode::Copied => EntryKind::Copy,
                    _ => EntryKind::Ordinary,
                };

                entries.push(StatusEntry {
                    path,
                    staged,
                    unstaged,
                    kind,
                    orig_path: Some(orig_path),
                });
            }
            '?' => {
                let path = record.strip_prefix("? ").unwrap_or(record).to_string();

                entries.push(StatusEntry {
                    path,
                    staged: StatusCode::Unmodified,
                    unstaged: StatusCode::Untracked,
                    kind: EntryKind::Untracked,
                    orig_path: None,
                });
            }
            '!' => {
                let path = record.strip_prefix("! ").unwrap_or(record).to_string();

                entries.push(StatusEntry {
                    path,
                    staged: StatusCode::Unmodified,
                    unstaged: StatusCode::Ignored,
                    kind: EntryKind::Ignored,
                    orig_path: None,
                });
            }
            other => {
                return Err(GitAiError::Generic(format!(
                    "Unsupported porcelain v2 record tag: {}",
                    other
                )));
            }
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_debug_snapshot;

    #[test]
    fn parse_varied_porcelain_v2_records() {
        // Construct a blob of porcelain v2 entries covering tracked, renamed, copied,
        // unmerged, untracked, and ignored states with spaces and special characters.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"1 MM N... 100644 100644 100644 1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 src/lib.rs\0");
        raw.extend_from_slice(b"1 AM N... 100644 100755 100755 3333333333333333333333333333333333333333 4444444444444444444444444444444444444444 src/bin/cli.rs\0");
        raw.extend_from_slice(b"1 .U N... 100644 100644 100644 5555555555555555555555555555555555555555 6666666666666666666666666666666666666666 src/conflict.rs\0");
        raw.extend_from_slice(b"2 R. N... 100644 100644 100644 7777777777777777777777777777777777777777 8888888888888888888888888888888888888888 80 src/utils/helpers.rs\0old utils/helpers.rs\0");
        raw.extend_from_slice(b"2 C. N... 100644 100644 100644 9999999999999999999999999999999999999999 0000000000000000000000000000000000000000 60 scripts/setup.sh\0scripts/setup-old.sh\0");
        raw.extend_from_slice(b"1 D. N... 100644 000000 000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0000000000000000000000000000000000000000 docs/README.md\0");
        raw.extend_from_slice(b"1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \"space dir\"/new file.txt\0");
        raw.extend_from_slice(b"1 M. N... 100644 100644 100644 cccccccccccccccccccccccccccccccccccccccc dddddddddddddddddddddddddddddddddddddddd path/with->symbol.rs\0");
        raw.extend_from_slice(b"? assets/logo (1).svg\0");
        raw.extend_from_slice(b"? dir with spaces/file name [draft].md\0");
        raw.extend_from_slice(b"! target/.keep\0");
        raw.extend_from_slice(b"u UU N... 100644 100644 100644 eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee ffffffffffffffffffffffffffffffffffffffff 1 2 3 some unmerged/path.txt\0");

        let entries: Vec<StatusEntry> = parse_porcelain_v2(&raw).expect("parse succeeds");

        // High-level assertions about the parsed content
        assert_eq!(entries.len(), 12);
        assert!(
            entries
                .iter()
                .any(|e| e.path == "src/lib.rs" && e.staged == StatusCode::Modified)
        );
        assert!(entries.iter().any(|e| e.kind == EntryKind::Rename
            && e.orig_path.as_deref() == Some("old utils/helpers.rs")));
        assert!(
            entries.iter().any(|e| e.kind == EntryKind::Copy
                && e.orig_path.as_deref() == Some("scripts/setup-old.sh"))
        );
        assert!(entries.iter().any(|e| e.kind == EntryKind::Unmerged));
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.unstaged, StatusCode::Untracked))
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(e.unstaged, StatusCode::Ignored))
        );

        assert_debug_snapshot!(entries);
    }
}
