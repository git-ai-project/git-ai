use regex::Regex;

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::rebase_authorship::rewrite_authorship_if_needed;
use crate::config;
use crate::error::GitAiError;
use crate::git::refs::get_authorship;
use crate::git::repo_storage::RepoStorage;
use crate::git::rewrite_log::RewriteLogEvent;
use crate::git::status::MAX_PATHSPEC_ARGS;
use crate::git::sync_authorship::{fetch_authorship_notes, push_authorship_notes};
#[cfg(windows)]
use crate::utils::is_interactive_terminal;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(windows)]
use crate::utils::CREATE_NO_WINDOW;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub struct Object<'a> {
    repo: &'a Repository,
    oid: String,
}

impl<'a> Object<'a> {
    pub fn id(&self) -> String {
        self.oid.clone()
    }

    // Recursively peel an object until a commit is found.
    pub fn peel_to_commit(&self) -> Result<Commit<'a>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-parse".to_string());
        // args.push("-q".to_string());
        args.push("--verify".to_string());
        args.push(format!("{}^{}", self.oid, "{commit}"));
        let output = exec_git(&args)?;
        Ok(Commit {
            repo: self.repo,
            oid: String::from_utf8(output.stdout)?.trim().to_string(),
            authorship_log: std::cell::OnceCell::new(),
        })
    }
}

#[derive(Debug, Clone)]

pub struct CommitRange<'a> {
    repo: &'a Repository,
    pub start_oid: String,
    pub end_oid: String,
    pub refname: String,
}

impl<'a> CommitRange<'a> {
    /// Create an empty CommitRange with no commits in its iterator.
    #[allow(dead_code)]
    pub fn empty(repo: &'a Repository) -> Self {
        Self {
            repo,
            start_oid: String::new(),
            end_oid: String::new(),
            refname: String::new(),
        }
    }

    #[allow(dead_code)]
    pub fn new(
        repo: &'a Repository,
        start_oid: String,
        end_oid: String,
        refname: String,
    ) -> Result<Self, GitAiError> {
        // Resolve start_oid and end_oid to actual commit SHAs
        let resolved_start = repo.revparse_single(&start_oid)?.oid;
        let resolved_end = repo.revparse_single(&end_oid)?.oid;

        Ok(Self {
            repo,
            start_oid: resolved_start,
            end_oid: resolved_end,
            refname,
        })
    }

    /// Create a new CommitRange with automatic refname inference.
    /// If refname is None, tries to find a single ref pointing to end_oid.
    /// If exactly one ref is found, uses that. Otherwise falls back to current HEAD.
    pub fn new_infer_refname(
        repo: &'a Repository,
        start_oid: String,
        end_oid: String,
        refname: Option<String>,
    ) -> Result<Self, GitAiError> {
        // Resolve start_oid and end_oid to actual commit SHAs
        let resolved_start = repo.revparse_single(&start_oid)?.oid;
        let resolved_end = repo.revparse_single(&end_oid)?.oid;

        let inferred_refname = match refname {
            Some(name) => name,
            None => {
                // Try to find refs pointing to resolved end_oid
                let mut args = repo.global_args_for_exec();
                args.push("for-each-ref".to_string());
                args.push("--points-at".to_string());
                args.push(resolved_end.clone());
                args.push("--format=%(refname)".to_string());

                let refs = match exec_git(&args) {
                    Ok(output) => {
                        let stdout = String::from_utf8(output.stdout).unwrap_or_default();
                        let refs: Vec<String> = stdout
                            .lines()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        refs
                    }
                    Err(_) => Vec::new(),
                };

                // If exactly one ref found, use it
                if refs.len() == 1 {
                    refs[0].clone()
                } else {
                    // Fall back to current HEAD
                    match repo.head() {
                        Ok(head_ref) => head_ref.name().unwrap_or("HEAD").to_string(),
                        Err(_) => "HEAD".to_string(),
                    }
                }
            }
        };

        Ok(Self {
            repo,
            start_oid: resolved_start,
            end_oid: resolved_end,
            refname: inferred_refname,
        })
    }

    pub fn repo(&self) -> &'a Repository {
        self.repo
    }

    pub fn is_valid(&self) -> Result<(), GitAiError> {
        const EMPTY_TREE_HASH: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

        // Check that both commits exist
        // Skip validation for empty tree hash - it's a special git object that may not exist in the repo
        if self.start_oid != EMPTY_TREE_HASH {
            self.repo.find_commit(self.start_oid.clone())?;
        }
        self.repo.find_commit(self.end_oid.clone())?;

        // Check that both commits exist on the refname
        // Use git merge-base --is-ancestor <commit> <refname>
        // Skip merge-base check for empty tree hash since it's not part of commit history
        if self.start_oid != EMPTY_TREE_HASH {
            let mut args = self.repo.global_args_for_exec();
            args.push("merge-base".to_string());
            args.push("--is-ancestor".to_string());
            args.push(self.start_oid.clone());
            args.push(self.refname.clone());

            exec_git(&args).map_err(|_| {
                GitAiError::Generic(format!(
                    "Commit {} is not reachable from refname {}",
                    self.start_oid, self.refname
                ))
            })?;
        }

        let mut args = self.repo.global_args_for_exec();
        args.push("merge-base".to_string());
        args.push("--is-ancestor".to_string());
        args.push(self.end_oid.clone());
        args.push(self.refname.clone());

        exec_git(&args).map_err(|_| {
            GitAiError::Generic(format!(
                "Commit {} is not reachable from refname {}",
                self.end_oid, self.refname
            ))
        })?;

        // Check that start is an ancestor of end (direct path between them)
        // Skip for empty tree hash - it's not part of the commit DAG
        if self.start_oid != EMPTY_TREE_HASH {
            let mut args = self.repo.global_args_for_exec();
            args.push("merge-base".to_string());
            args.push("--is-ancestor".to_string());
            args.push(self.start_oid.clone());
            args.push(self.end_oid.clone());

            exec_git(&args).map_err(|_| {
                GitAiError::Generic(format!(
                    "Commit {} is not an ancestor of {}",
                    self.start_oid, self.end_oid
                ))
            })?;
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn length(&self) -> usize {
        // Use git rev-list --count to get the number of commits between start and end
        // Format: start_oid..end_oid means commits reachable from end_oid but not from start_oid
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-list".to_string());
        args.push("--count".to_string());
        args.push(format!("{}..{}", self.start_oid, self.end_oid));

        match exec_git(&args) {
            Ok(output) => {
                let count_str = String::from_utf8(output.stdout).unwrap_or_default();
                count_str.trim().parse().unwrap_or(0)
            }
            Err(_) => 0, // If they don't share lineage or error occurs, return 0
        }
    }

    pub fn all_commits(&self) -> Vec<String> {
        let mut commits = Vec::new();
        let itt = self.clone().into_iter();

        for commit in itt {
            commits.push(commit.oid.clone());
        }
        commits
    }
}

impl<'a> IntoIterator for CommitRange<'a> {
    type Item = Commit<'a>;
    type IntoIter = CommitRangeIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        // Empty range - return empty iterator
        if self.start_oid.is_empty() && self.end_oid.is_empty() {
            return CommitRangeIterator {
                repo: self.repo,
                commit_oids: Vec::new(),
                index: 0,
            };
        }

        // ie for single commit branches
        if self.start_oid == self.end_oid {
            return CommitRangeIterator {
                repo: self.repo,
                commit_oids: vec![self.end_oid.clone()],
                index: 0,
            };
        }

        // Use git rev-list to get all commits between start and end
        // Format: start_oid..end_oid means commits reachable from end_oid but not from start_oid
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-list".to_string());
        args.push(format!("{}..{}", self.start_oid, self.end_oid));

        let commit_oids: Vec<String> = match exec_git(&args) {
            Ok(output) => {
                let stdout = String::from_utf8(output.stdout).unwrap_or_default();
                stdout
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            Err(_) => Vec::new(), // If they don't share lineage or error occurs, return empty
        };

        CommitRangeIterator {
            repo: self.repo,
            commit_oids,
            index: 0,
        }
    }
}

pub struct CommitRangeIterator<'a> {
    repo: &'a Repository,
    commit_oids: Vec<String>,
    index: usize,
}

impl<'a> Iterator for CommitRangeIterator<'a> {
    type Item = Commit<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.commit_oids.len() {
            return None;
        }
        let oid = self.commit_oids[self.index].clone();
        self.index += 1;
        Some(Commit {
            repo: self.repo,
            oid,
            authorship_log: std::cell::OnceCell::new(),
        })
    }
}

pub struct Signature<'a> {
    #[allow(dead_code)]
    repo: &'a Repository,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    email: String,
    time_iso8601: String,
}

pub struct Time {
    seconds: i64,
    #[allow(dead_code)]
    offset_minutes: i32,
}

impl Time {
    pub fn seconds(&self) -> i64 {
        self.seconds
    }

    #[allow(dead_code)]
    pub fn offset_minutes(&self) -> i32 {
        self.offset_minutes
    }
}

impl<'a> Signature<'a> {
    #[allow(dead_code)]
    pub fn name(&self) -> Option<&str> {
        if self.name.is_empty() {
            None
        } else {
            Some(self.name.as_str())
        }
    }

    #[allow(dead_code)]
    pub fn email(&self) -> Option<&str> {
        if self.email.is_empty() {
            None
        } else {
            Some(self.email.as_str())
        }
    }

    pub fn when(&self) -> Time {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&self.time_iso8601) {
            let seconds = dt.timestamp();
            let offset_minutes = dt.offset().local_minus_utc() / 60;
            Time {
                seconds,
                offset_minutes,
            }
        } else {
            // TODO Log error
            // Fallback to epoch if parsing fails
            Time {
                seconds: 0,
                offset_minutes: 0,
            }
        }
    }
}

pub struct Commit<'a> {
    repo: &'a Repository,
    oid: String,
    #[allow(dead_code)]
    authorship_log: std::cell::OnceCell<AuthorshipLog>,
}

impl<'a> Commit<'a> {
    pub fn id(&self) -> String {
        self.oid.clone()
    }

    pub fn tree(&self) -> Result<Tree<'a>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-parse".to_string());
        // args.push("-q".to_string());
        args.push("--verify".to_string());
        args.push(format!("{}^{}", self.oid, "{tree}"));
        let output = exec_git(&args)?;
        Ok(Tree {
            repo: self.repo,
            oid: String::from_utf8(output.stdout)?.trim().to_string(),
        })
    }

    pub fn parent(&self, i: usize) -> Result<Commit<'a>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-parse".to_string());
        // args.push("-q".to_string());
        args.push("--verify".to_string());
        // libgit2 uses 0-based indexing; Git's rev syntax uses 1-based parent selectors.
        args.push(format!("{}^{}", self.oid, i + 1));
        let output = exec_git(&args)?;
        Ok(Commit {
            repo: self.repo,
            oid: String::from_utf8(output.stdout)?.trim().to_string(),
            authorship_log: std::cell::OnceCell::new(),
        })
    }

    // Return an iterator over the parents of this commit.
    pub fn parents(&self) -> Parents<'a> {
        // Use `git show -s --format=%P <oid>` to get whitespace-separated parent OIDs
        let mut args = self.repo.global_args_for_exec();
        args.push("show".to_string());
        args.push("-s".to_string());
        args.push("--format=%P".to_string());
        args.push(self.oid.clone());

        let parent_oids: Vec<String> = match exec_git(&args) {
            Ok(output) => {
                let stdout = String::from_utf8(output.stdout).unwrap_or_default();
                stdout.split_whitespace().map(|s| s.to_string()).collect()
            }
            Err(_) => Vec::new(),
        };

        Parents {
            repo: self.repo,
            parent_oids,
            index: 0,
        }
    }

    // Get the number of parents of this commit.
    // Use the parents iterator to return an iterator over all parents.
    #[allow(dead_code)]
    pub fn parent_count(&self) -> Result<usize, GitAiError> {
        Ok(self.parents().count())
    }

    // Get the short "summary" of the git commit message. The returned message is the summary of the commit, comprising the first paragraph of the message with whitespace trimmed and squashed. None may be returned if an error occurs or if the summary is not valid utf-8.
    pub fn summary(&self) -> Result<String, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("show".to_string());
        args.push("-s".to_string());
        args.push("--no-notes".to_string());
        args.push("--encoding=UTF-8".to_string());
        args.push("--format=%s".to_string());
        args.push(self.oid.clone());
        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    // Get the body of the git commit message (everything after the first paragraph).
    // Returns an empty string if there is no body.
    pub fn body(&self) -> Result<String, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("show".to_string());
        args.push("-s".to_string());
        args.push("--no-notes".to_string());
        args.push("--encoding=UTF-8".to_string());
        args.push("--format=%b".to_string());
        args.push(self.oid.clone());
        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    // Get the author of this commit.
    #[allow(dead_code)]
    pub fn author(&self) -> Result<Signature<'a>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("show".to_string());
        args.push("-s".to_string());
        args.push("--no-notes".to_string());
        args.push("--encoding=UTF-8".to_string());
        args.push("--format=%an%n%ae%n%aI".to_string());
        args.push(self.oid.clone());
        let output = exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)?;
        let mut lines = stdout.lines();
        let name = lines.next().unwrap_or("").trim().to_string();
        let email = lines.next().unwrap_or("").trim().to_string();
        let time_iso8601 = lines.next().unwrap_or("").trim().to_string();
        Ok(Signature {
            repo: self.repo,
            name,
            email,
            time_iso8601,
        })
    }

    // Get the committer of this commit.
    #[allow(dead_code)]
    pub fn committer(&self) -> Result<Signature<'a>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("show".to_string());
        args.push("-s".to_string());
        args.push("--no-notes".to_string());
        args.push("--encoding=UTF-8".to_string());
        args.push("--format=%cn%n%ce%n%cI".to_string());
        args.push(self.oid.clone());
        let output = exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)?;
        let mut lines = stdout.lines();
        let name = lines.next().unwrap_or("").trim().to_string();
        let email = lines.next().unwrap_or("").trim().to_string();
        let time_iso8601 = lines.next().unwrap_or("").trim().to_string();
        Ok(Signature {
            repo: self.repo,
            name,
            email,
            time_iso8601,
        })
    }

    // Get the commit time (i.e. committer time) of a commit.
    // The first element of the tuple is the time, in seconds, since the epoch. The second element is the offset, in minutes, of the time zone of the committer's preferred time zone.
    #[allow(dead_code)]
    pub fn time(&self) -> Result<Time, GitAiError> {
        let signature = self.committer()?;
        Ok(signature.when())
    }

    // lazy load the authorship log
    #[allow(dead_code)]
    pub fn authorship(&self) -> &AuthorshipLog {
        self.authorship_log
            .get_or_init(|| get_authorship(self.repo, self.oid.as_str()).unwrap_or_default())
    }
    #[allow(dead_code)]
    pub fn authorship_uncached(&self) -> AuthorshipLog {
        get_authorship(self.repo, self.oid.as_str()).unwrap_or_default()
    }

    /// Find the first parent that exists on the specified refname
    ///
    /// This is useful for merge commits where we want to find the parent on a specific branch
    /// (e.g., main) rather than just taking the first parent, which might not be correct in
    /// complex merge histories with back-and-forth merges.
    ///
    /// # Arguments
    /// * `refname` - The reference name to search for (e.g., "main", "refs/heads/main")
    ///
    /// # Returns
    /// The first parent commit that is reachable from the specified refname
    pub fn parent_on_refname(&self, refname: &str) -> Result<Commit<'a>, GitAiError> {
        // Normalize the refname to fully qualified form
        let fq_refname = {
            let mut rp_args = self.repo.global_args_for_exec();
            rp_args.push("rev-parse".to_string());
            rp_args.push("--verify".to_string());
            rp_args.push("--symbolic-full-name".to_string());
            rp_args.push(refname.to_string());

            match exec_git(&rp_args) {
                Ok(output) => {
                    let s = String::from_utf8(output.stdout).unwrap_or_default();
                    let s = s.trim();
                    if s.is_empty() {
                        if refname.starts_with("refs/") {
                            refname.to_string()
                        } else {
                            format!("refs/heads/{}", refname)
                        }
                    } else {
                        s.to_string()
                    }
                }
                Err(_) => {
                    if refname.starts_with("refs/") {
                        refname.to_string()
                    } else {
                        format!("refs/heads/{}", refname)
                    }
                }
            }
        };

        // Iterate through parents and find the first one that's on the refname
        for parent in self.parents() {
            let parent_sha = parent.id();

            // Check if this parent is an ancestor of the refname
            // git merge-base --is-ancestor <parent> <refname>
            let mut args = self.repo.global_args_for_exec();
            args.push("merge-base".to_string());
            args.push("--is-ancestor".to_string());
            args.push(parent_sha.clone());
            args.push(fq_refname.clone());

            if exec_git(&args).is_ok() {
                return Ok(parent);
            }
        }

        // If no parent is on the refname, return an error
        Err(GitAiError::Generic(format!(
            "No parent of commit {} is reachable from refname {}",
            self.oid, refname
        )))
    }
}

pub struct TreeEntry<'a> {
    #[allow(dead_code)]
    repo: &'a Repository,
    // Object id (SHA-1/oid) that this tree entry points to
    oid: String,
    // One of: blob, tree, commit (gitlink)
    #[allow(dead_code)]
    object_type: String,
    // File mode as provided by git ls-tree (e.g. 100644, 100755, 120000, 040000)
    #[allow(dead_code)]
    mode: String,
    // Full path relative to the root of the tree used for lookup
    #[allow(dead_code)]
    path: String,
}

impl<'a> TreeEntry<'a> {
    // Get the id of the object pointed by the entry
    pub fn id(&self) -> String {
        self.oid.clone()
    }
}

pub struct Tree<'a> {
    repo: &'a Repository,
    oid: String,
}

impl<'a> Tree<'a> {
    // Get the id of the tree
    pub fn id(&self) -> String {
        self.oid.clone()
    }

    #[allow(dead_code)]
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Tree<'a> {
        Tree {
            repo: self.repo,
            oid: self.oid.clone(),
        }
    }

    // Retrieve a tree entry contained in a tree or in any of its subtrees, given its relative path.
    pub fn get_path(&self, path: &Path) -> Result<TreeEntry<'a>, GitAiError> {
        // Use `git ls-tree -z -d <tree-oid> -- <path>` to get exactly the entry for the path.
        // -z ensures NUL-terminated records; -d shows the directory itself instead of listing contents
        let mut args = self.repo.global_args_for_exec();
        args.push("ls-tree".to_string());
        args.push("-z".to_string());
        // Use recursive to locate files in nested paths and return blob entries
        args.push("-r".to_string());
        args.push(self.oid.clone());
        args.push("--".to_string());
        let path_str = path.to_string_lossy().to_string();
        args.push(path_str.clone());

        let output = exec_git(&args)?;
        let bytes = output.stdout;

        // Each record: "<mode> <type> <object>\t<file>\0"
        // We expect at most one record for an exact path query.
        let mut found_entry: Option<TreeEntry<'a>> = None;

        for chunk in bytes.split(|b| *b == 0u8) {
            if chunk.is_empty() {
                continue;
            }
            // Split metadata and path on first tab
            let mut parts = chunk.splitn(2, |b| *b == b'\t');
            let meta = parts.next().unwrap_or(&[]);
            let file_bytes = parts.next().unwrap_or(&[]);

            // Parse meta: "<mode> <type> <object>"
            let meta_str = String::from_utf8_lossy(meta);
            let mut meta_iter = meta_str.split_whitespace();
            let mode = meta_iter.next().unwrap_or("").to_string();
            let object_type = meta_iter.next().unwrap_or("").to_string();
            let oid = meta_iter.next().unwrap_or("").to_string();

            if mode.is_empty() || object_type.is_empty() || oid.is_empty() {
                continue;
            }

            let file_path = String::from_utf8_lossy(file_bytes).to_string();

            // Prefer exact path match if multiple records somehow appear
            if found_entry.is_none() || file_path == path_str {
                found_entry = Some(TreeEntry {
                    repo: self.repo,
                    oid,
                    object_type,
                    mode,
                    path: file_path,
                });
            }
        }

        match found_entry {
            Some(entry) => Ok(entry),
            None => Err(GitAiError::Generic(format!(
                "Path not found in tree: {}",
                path.to_string_lossy()
            ))),
        }
    }
}

pub struct Blob<'a> {
    repo: &'a Repository,
    oid: String,
}

impl<'a> Blob<'a> {
    #[allow(dead_code)]
    pub fn id(&self) -> String {
        self.oid.clone()
    }

    // Get the content of this blob.
    pub fn content(&self) -> Result<Vec<u8>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("cat-file".to_string());
        args.push("blob".to_string());
        args.push(self.oid.clone());
        let output = exec_git(&args)?;
        Ok(output.stdout)
    }
}

pub struct Reference<'a> {
    repo: &'a Repository,
    ref_name: String,
}

impl<'a> Reference<'a> {
    pub fn name(&self) -> Option<&str> {
        Some(&self.ref_name)
    }

    #[allow(dead_code)]
    pub fn is_branch(&self) -> bool {
        self.ref_name.starts_with("refs/heads/")
    }

    pub fn shorthand(&self) -> Result<String, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-parse".to_string());
        args.push("--abbrev-ref".to_string());
        args.push(self.ref_name.clone());
        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    pub fn target(&self) -> Result<String, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-parse".to_string());
        args.push(self.ref_name.clone());
        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    // Peel a reference to a blob
    // This method recursively peels the reference until it reaches a blob.
    #[allow(dead_code)]
    pub fn peel_to_blob(&self) -> Result<Blob<'a>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-parse".to_string());
        // args.push("-q".to_string());
        args.push("--verify".to_string());
        args.push(format!("{}^{}", self.ref_name, "{blob}"));
        let output = exec_git(&args)?;
        Ok(Blob {
            repo: self.repo,
            oid: String::from_utf8(output.stdout)?.trim().to_string(),
        })
    }

    // Peel a reference to a commit This method recursively peels the reference until it reaches a commit.
    #[allow(dead_code)]
    pub fn peel_to_commit(&self) -> Result<Commit<'a>, GitAiError> {
        let mut args = self.repo.global_args_for_exec();
        args.push("rev-parse".to_string());
        // args.push("-q".to_string());
        args.push("--verify".to_string());
        args.push(format!("{}^{}", self.ref_name, "{commit}"));
        let output = exec_git(&args)?;
        Ok(Commit {
            repo: self.repo,
            oid: String::from_utf8(output.stdout)?.trim().to_string(),
            authorship_log: std::cell::OnceCell::new(),
        })
    }
}

pub struct Parents<'a> {
    repo: &'a Repository,
    parent_oids: Vec<String>,
    index: usize,
}

impl<'a> Iterator for Parents<'a> {
    type Item = Commit<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.parent_oids.len() {
            return None;
        }
        let oid = self.parent_oids[self.index].clone();
        self.index += 1;
        Some(Commit {
            repo: self.repo,
            oid,
            authorship_log: std::cell::OnceCell::new(),
        })
    }
}

pub struct References<'a> {
    repo: &'a Repository,
    refs: Vec<String>,
    index: usize,
}

impl<'a> Iterator for References<'a> {
    type Item = Result<Reference<'a>, GitAiError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.refs.len() {
            return None;
        }
        let ref_name = self.refs[self.index].clone();
        self.index += 1;
        Some(Ok(Reference {
            repo: self.repo,
            ref_name,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct Repository {
    global_args: Vec<String>,
    git_dir: PathBuf,
    pub storage: RepoStorage,
    pub pre_command_base_commit: Option<String>,
    pub pre_command_refname: Option<String>,
    pub pre_reset_target_commit: Option<String>,
    workdir: PathBuf,
    /// Canonical (absolute, resolved) version of workdir for reliable path comparisons
    /// On Windows, this uses the \\?\ UNC prefix format
    canonical_workdir: PathBuf,
}

impl Repository {
    // Util for preparing global args for execution
    pub fn global_args_for_exec(&self) -> Vec<String> {
        let mut args = self.global_args.clone();
        if !args.iter().any(|arg| arg == "--no-pager") {
            args.push("--no-pager".to_string());
        }
        args
    }

    /// Execute an arbitrary git command and return stdout as string
    #[allow(dead_code)]
    pub fn git(&self, args: &[&str]) -> Result<String, GitAiError> {
        let mut full_args = self.global_args_for_exec();
        full_args.extend(args.iter().map(|s| s.to_string()));
        let output = exec_git(&full_args)?;
        Ok(String::from_utf8(output.stdout)?)
    }

    pub fn require_pre_command_head(&mut self) {
        if self.pre_command_base_commit.is_some() || self.pre_command_refname.is_some() {
            return;
        }

        // Safely handle empty repositories
        if let Ok(head_ref) = self.head()
            && let Ok(target) = head_ref.target()
        {
            let target_string = target;
            let refname = head_ref.name().map(|n| n.to_string());
            self.pre_command_base_commit = Some(target_string);
            self.pre_command_refname = refname;
        }
    }

    pub fn handle_rewrite_log_event(
        &mut self,
        rewrite_log_event: RewriteLogEvent,
        commit_author: String,
        supress_output: bool,
        apply_side_effects: bool,
    ) {
        let log = self
            .storage
            .append_rewrite_event(rewrite_log_event.clone())
            .expect("Error writing .git/ai/rewrite_log");

        if apply_side_effects
            && let Ok(_) = rewrite_authorship_if_needed(
                self,
                &rewrite_log_event,
                commit_author,
                &log,
                supress_output,
            )
        {}
    }

    // Internal util to get the git object type for a given OID
    fn object_type(&self, oid: &str) -> Result<String, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("cat-file".to_string());
        args.push("-t".to_string());
        args.push(oid.to_string());
        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    // Retrieve and resolve the reference pointed at by HEAD.
    // If HEAD is a symbolic ref, return the refname (e.g., "refs/heads/main").
    // Otherwise, return "HEAD".
    pub fn head<'a>(&'a self) -> Result<Reference<'a>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("symbolic-ref".to_string());
        // args.push("-q".to_string());
        args.push("HEAD".to_string());

        let output = exec_git(&args);

        match output {
            Ok(output) if output.status.success() => {
                let refname = String::from_utf8(output.stdout)?;
                Ok(Reference {
                    repo: self,
                    ref_name: refname.trim().to_string(),
                })
            }
            _ => Ok(Reference {
                repo: self,
                ref_name: "HEAD".to_string(),
            }),
        }
    }

    // Returns the path to the .git folder for normal repositories or the repository itself for bare repositories.
    // TODO Test on bare repositories.
    pub fn path(&self) -> &Path {
        self.git_dir.as_path()
    }

    // Get the path of the working directory for this repository.
    // If this repository is bare, then None is returned.
    pub fn workdir(&self) -> Result<PathBuf, GitAiError> {
        // TODO Remove Result since this is determined at initialization now
        Ok(self.workdir.clone())
    }

    /// Returns true when this repository is bare.
    pub fn is_bare_repository(&self) -> Result<bool, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("rev-parse".to_string());
        args.push("--is-bare-repository".to_string());
        let output = exec_git(&args)?;
        let value = String::from_utf8(output.stdout)?;
        Ok(value.trim() == "true")
    }

    /// Get the canonical (absolute, resolved) path of the working directory
    /// On Windows, this uses the \\?\ UNC prefix format for reliable path comparisons
    #[allow(dead_code)]
    pub fn canonical_workdir(&self) -> &Path {
        &self.canonical_workdir
    }

    /// Check if a path is within the repository's working directory
    /// Uses canonical path comparison for reliability on Windows
    pub fn path_is_in_workdir(&self, path: &Path) -> bool {
        // Try canonical comparison first (most reliable, especially on Windows)
        if let Ok(canonical_path) = path.canonicalize() {
            return canonical_path.starts_with(&self.canonical_workdir);
        }

        // Fallback for paths that don't exist yet: normalize by resolving .. and .
        let normalized = path
            .components()
            .fold(std::path::PathBuf::new(), |mut acc, component| {
                match component {
                    std::path::Component::ParentDir => {
                        acc.pop();
                    }
                    std::path::Component::CurDir => {}
                    _ => acc.push(component),
                }
                acc
            });
        normalized.starts_with(&self.workdir)
    }

    // List all remotes for a given repository
    pub fn remotes(&self) -> Result<Vec<String>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("remote".to_string());

        let output = exec_git(&args)?;
        let remotes = String::from_utf8(output.stdout)?;
        Ok(remotes.trim().split("\n").map(|s| s.to_string()).collect())
    }

    // List all remotes with their URLs as tuples (name, url)
    pub fn remotes_with_urls(&self) -> Result<Vec<(String, String)>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("remote".to_string());
        args.push("-v".to_string());

        let output = exec_git(&args)?;
        let remotes_output = String::from_utf8(output.stdout)?;

        let mut remotes = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for line in remotes_output.trim().split("\n").filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[0].to_string();
                let url = parts[1].to_string();
                // Only add each remote once (git remote -v shows fetch and push)
                if seen.insert(name.clone()) {
                    remotes.push((name, url));
                }
            }
        }

        Ok(remotes)
    }

    /// Get the git config file for this repository and fallback to global config if not found.
    fn get_git_config_file(&self) -> Result<gix_config::File<'static>, GitAiError> {
        match gix_config::File::from_git_dir(self.path().to_path_buf()) {
            Ok(git_config_file) => Ok(git_config_file),
            Err(e) => match gix_config::File::from_globals() {
                Ok(system_config) => Ok(system_config),
                Err(_) => Err(GitAiError::GixError(e.to_string())),
            },
        }
    }
    /// Get config value for a given key as a String.
    pub fn config_get_str(&self, key: &str) -> Result<Option<String>, GitAiError> {
        match self.get_git_config_file() {
            Ok(git_config_file) => Ok(git_config_file.string(key).map(|cow| cow.to_string())),
            Err(e) => Err(e),
        }
    }

    /// Get all config values matching a regex pattern.
    ///
    /// Regular expression matching is currently case-sensitive
    /// and done against a canonicalized version of the key
    /// in which section and variable names are lowercased, but subsection names are not.
    ///
    /// Returns a HashMap of key -> value for all matching config entries.
    pub fn config_get_regexp(
        &self,
        pattern: &str,
    ) -> Result<std::collections::HashMap<String, String>, GitAiError> {
        match self.get_git_config_file() {
            Ok(git_config_file) => {
                let mut matches: HashMap<String, String> = HashMap::new();

                let re = Regex::new(pattern)
                    .map_err(|e| GitAiError::Generic(format!("Invalid regex pattern: {}", e)))?;

                // iterate over all sections
                for section in git_config_file.sections() {
                    // Support subsections in the key
                    let section_name = section.header().name().to_string().to_lowercase();
                    let subsection = section.header().subsection_name();

                    for value_name in section.body().value_names() {
                        let value_name_str = value_name.to_string().to_lowercase();
                        let full_key = if let Some(sub) = subsection {
                            format!("{}.{}.{}", section_name, sub, value_name_str)
                        } else {
                            format!("{}.{}", section_name, value_name_str)
                        };

                        if re.is_match(&full_key)
                            && let Some(value) =
                                section.body().value(value_name).map(|c| c.to_string())
                        {
                            matches.insert(full_key, value);
                        }
                    }
                }
                Ok(matches)
            }
            Err(e) => Err(e),
        }
    }

    /// Get the git version as a tuple (major, minor, patch).
    /// Returns None if the version cannot be parsed.
    pub fn git_version(&self) -> Option<(u32, u32, u32)> {
        let args = vec!["--version".to_string()];
        let output = exec_git(&args).ok()?;
        let version_str = String::from_utf8(output.stdout).ok()?;
        parse_git_version(&version_str)
    }

    /// Check if the current git version supports --ignore-revs-file flag for blame.
    /// This flag was added in git 2.23.0.
    pub fn git_supports_ignore_revs_file(&self) -> bool {
        if let Some((major, minor, _)) = self.git_version() {
            // --ignore-revs-file was added in git 2.23.0
            major > 2 || (major == 2 && minor >= 23)
        } else {
            // If we can't determine the version, assume it's supported
            // to avoid breaking existing functionality
            true
        }
    }

    // Write an in-memory buffer to the ODB as a blob.
    // The Oid returned can in turn be passed to find_blob to get a handle to the blob.
    #[allow(dead_code)]
    pub fn blob(&self, data: &[u8]) -> Result<String, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("hash-object".to_string());
        args.push("-w".to_string());
        args.push("--stdin".to_string());
        let output = exec_git_stdin(&args, data)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    // Create a new direct reference. This function will return an error if a reference already exists with the given name unless force is true, in which case it will be overwritten.
    #[allow(dead_code)]
    pub fn reference<'a>(
        &'a self,
        name: &str,
        id: String,
        force: bool,
        log_message: &str,
    ) -> Result<Reference<'a>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("update-ref".to_string());
        args.push("--stdin".to_string());
        args.push("--create-reflog".to_string());
        args.push("-m".to_string());
        args.push(log_message.to_string());

        let verb = if force { "update" } else { "create" };
        let stdin_line = format!("{} {} {}\n", verb, name, id.trim());
        exec_git_stdin(&args, stdin_line.as_bytes())?;

        Ok(Reference {
            repo: self,
            ref_name: name.to_string(),
        })
    }

    #[allow(dead_code)]
    pub fn remote_head(&self, remote_name: &str) -> Result<String, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("symbolic-ref".to_string());
        args.push(format!("refs/remotes/{}/HEAD", remote_name));
        args.push("--short".to_string());

        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    // Lookup a reference to one of the objects in a repository. Requires full ref name.
    #[allow(dead_code)]
    pub fn find_reference(&self, name: &str) -> Result<Reference<'_>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("show-ref".to_string());
        args.push("--verify".to_string());
        args.push("-s".to_string());
        args.push(name.to_string());
        exec_git(&args)?;
        Ok(Reference {
            repo: self,
            ref_name: name.to_string(),
        })
    }
    // Find a merge base between two commits
    pub fn merge_base(&self, one: String, two: String) -> Result<String, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("merge-base".to_string());
        args.push(one.to_string());
        args.push(two.to_string());
        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    // Merge two trees, producing an index that reflects the result of the merge. The index may be written as-is to the working directory or checked out. If the index is to be converted to a tree, the caller should resolve any conflicts that arose as part of the merge.
    #[allow(dead_code)]
    pub fn merge_trees_favor_ours(
        &self,
        ancestor_tree: &Tree<'_>,
        our_tree: &Tree<'_>,
        their_tree: &Tree<'_>,
    ) -> Result<String, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("merge-tree".to_string());
        args.push("--write-tree".to_string());
        args.push(format!("--merge-base={}", ancestor_tree.oid));
        args.push("-X".to_string());
        args.push("ours".to_string());
        args.push(our_tree.oid.to_string());
        args.push(their_tree.oid.to_string());
        let output = exec_git(&args)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    #[allow(dead_code)]
    pub fn commit_range_on_branch(
        &self,
        branch_refname: &str,
        merge_target_refname: &str,
    ) -> Result<CommitRange<'_>, GitAiError> {
        // Normalize the provided branch ref to fully qualified using rev-parse
        let fq_branch = {
            let mut rp_args = self.global_args_for_exec();
            rp_args.push("rev-parse".to_string());
            rp_args.push("--verify".to_string());
            rp_args.push("--symbolic-full-name".to_string());
            rp_args.push(branch_refname.to_string());

            match exec_git(&rp_args) {
                Ok(output) => {
                    let s = String::from_utf8(output.stdout).unwrap_or_default();
                    let s = s.trim();
                    if s.is_empty() {
                        if branch_refname.starts_with("refs/") {
                            branch_refname.to_string()
                        } else {
                            format!("refs/heads/{}", branch_refname)
                        }
                    } else {
                        s.to_string()
                    }
                }
                Err(_) => {
                    if branch_refname.starts_with("refs/") {
                        branch_refname.to_string()
                    } else {
                        format!("refs/heads/{}", branch_refname)
                    }
                }
            }
        };

        let fq_merge_target = {
            let mut rp_args = self.global_args_for_exec();
            rp_args.push("rev-parse".to_string());
            rp_args.push("--verify".to_string());
            rp_args.push("--symbolic-full-name".to_string());
            rp_args.push(merge_target_refname.to_string());

            match exec_git(&rp_args) {
                Ok(output) => {
                    let s = String::from_utf8(output.stdout).unwrap_or_default();
                    let s = s.trim();
                    if s.is_empty() {
                        if merge_target_refname.starts_with("refs/") {
                            merge_target_refname.to_string()
                        } else {
                            format!("refs/heads/{}", merge_target_refname)
                        }
                    } else {
                        s.to_string()
                    }
                }
                Err(_) => {
                    if merge_target_refname.starts_with("refs/") {
                        merge_target_refname.to_string()
                    } else {
                        format!("refs/heads/{}", merge_target_refname)
                    }
                }
            }
        };

        // Build: git log --format=%H --reverse <branch> --not <merge_target>
        // Note: we intentionally do NOT use --ancestry-path here. That flag requires
        // commits to be descendants of the merge-target's tip, which fails when the
        // merge target was previously merged INTO the branch (a common workflow to
        // stay up-to-date). In that case, the branch's unique commits descend from
        // the pre-merge side and --ancestry-path filters them all out.
        let mut log_args = self.global_args_for_exec();
        log_args.push("log".to_string());
        log_args.push("--format=%H".to_string());
        log_args.push("--reverse".to_string());
        log_args.push(fq_branch.to_string());
        log_args.push("--not".to_string());
        log_args.push(fq_merge_target.to_string());

        let log_output = exec_git(&log_args).map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to get commit log for {}: {:?}",
                branch_refname, e
            ))
        })?;

        let log_str = String::from_utf8(log_output.stdout)
            .map_err(|e| GitAiError::Generic(format!("Failed to parse log output: {:?}", e)))?;

        let commits: Vec<&str> = log_str.lines().filter(|line| !line.is_empty()).collect();

        if commits.is_empty() {
            return Err(GitAiError::Generic(format!(
                "No commits found on branch {} unique to this branch",
                branch_refname
            )));
        }

        let first_commit = commits.first().unwrap().to_string();
        let last_commit = commits.last().unwrap().to_string();

        CommitRange::new(self, first_commit, last_commit, fq_branch.to_string())
    }

    // Create new commit in the repository If the update_ref is not None, name of the reference that will be updated to point to this commit. If the reference is not direct, it will be resolved to a direct reference. Use "HEAD" to update the HEAD of the current branch and make it point to this commit. If the reference doesn't exist yet, it will be created. If it does exist, the first parent must be the tip of this branch.
    #[allow(dead_code)]
    pub fn commit(
        &self,
        update_ref: Option<&str>,
        author: &Signature<'_>,
        committer: &Signature<'_>,
        message: &str,
        tree: &Tree<'_>,
        parents: &[&Commit<'_>],
    ) -> Result<String, GitAiError> {
        // Validate identities
        let author_name = author.name().unwrap_or("").trim().to_string();
        let author_email = author.email().unwrap_or("").trim().to_string();
        let committer_name = committer.name().unwrap_or("").trim().to_string();
        let committer_email = committer.email().unwrap_or("").trim().to_string();

        if author_name.is_empty() || author_email.is_empty() {
            return Err(GitAiError::Generic(
                "Missing author name or email".to_string(),
            ));
        }
        if committer_name.is_empty() || committer_email.is_empty() {
            return Err(GitAiError::Generic(
                "Missing committer name or email".to_string(),
            ));
        }

        // Format dates as "<unix-seconds> <±HHMM>" which Git accepts
        let fmt_git_date = |t: Time| -> String {
            let seconds = t.seconds();
            let offset_min = t.offset_minutes();
            let sign = if offset_min >= 0 { '+' } else { '-' };
            let abs = offset_min.abs();
            let hh = abs / 60;
            let mm = abs % 60;
            format!("{} {}{:02}{:02}", seconds, sign, hh, mm)
        };
        let author_date = fmt_git_date(author.when());
        let committer_date = fmt_git_date(committer.when());

        // Build env for commit-tree
        let env: Vec<(String, String)> = vec![
            ("GIT_AUTHOR_NAME".to_string(), author_name),
            ("GIT_AUTHOR_EMAIL".to_string(), author_email),
            ("GIT_AUTHOR_DATE".to_string(), author_date),
            ("GIT_COMMITTER_NAME".to_string(), committer_name),
            ("GIT_COMMITTER_EMAIL".to_string(), committer_email),
            ("GIT_COMMITTER_DATE".to_string(), committer_date),
        ];

        // 1) Create the commit object via commit-tree, piping message on stdin
        let mut ct_args = self.global_args_for_exec();
        ct_args.push("commit-tree".to_string());
        ct_args.push(tree.oid.clone());
        for p in parents.iter() {
            ct_args.push("-p".to_string());
            ct_args.push(p.id());
        }
        let ct_out = exec_git_stdin_with_env(&ct_args, &env, message.as_bytes())?;
        let new_commit = String::from_utf8(ct_out.stdout)?.trim().to_string();

        // 2) Optionally update a ref with CAS semantics
        if let Some(update_ref_name) = update_ref {
            // Resolve target ref (HEAD may be symbolic)
            let target_ref = if update_ref_name == "HEAD" {
                // If HEAD is symbolic this returns e.g. refs/heads/main; otherwise "HEAD"
                self.head()?.name().unwrap().to_string()
            } else {
                update_ref_name.to_string()
            };

            // Capture current tip if any: rev-parse -q --verify <target_ref>
            let mut rp_args = self.global_args_for_exec();
            rp_args.push("rev-parse".to_string());
            // rp_args.push("-q".to_string()); // For gitai, we want to see the error message if the ref doesn't exist
            rp_args.push("--verify".to_string());
            rp_args.push(target_ref.clone());

            let old_tip: Option<String> = match Command::new(config::Config::get().git_cmd())
                .args(&rp_args)
                .output()
            {
                Ok(output) if output.status.success() => {
                    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                }
                _ => None,
            };

            // Enforce first-parent matches current tip if ref exists
            if let Some(ref tip) = old_tip {
                if parents.is_empty() {
                    return Err(GitAiError::Generic(
                        "Ref exists but no parents were provided".to_string(),
                    ));
                }
                let first_parent = parents[0].id();
                if first_parent.trim() != tip {
                    return Err(GitAiError::Generic(format!(
                        "First parent ({}) != current tip ({}) of {}",
                        first_parent, tip, target_ref
                    )));
                }
            }

            // Update the ref atomically (include OLD_TIP for CAS if present)
            let mut ur_args = self.global_args_for_exec();
            ur_args.push("update-ref".to_string());
            ur_args.push("-m".to_string());
            ur_args.push(message.to_string());
            ur_args.push(target_ref.clone());
            ur_args.push(new_commit.clone());
            if let Some(tip) = old_tip {
                ur_args.push(tip);
            }
            exec_git(&ur_args)?;
        }

        Ok(new_commit)
    }

    // Find a single object, as specified by a revision string.
    pub fn revparse_single(&self, spec: &str) -> Result<Object<'_>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("rev-parse".to_string());
        // args.push("-q".to_string());
        args.push("--verify".to_string());
        args.push(spec.to_string());
        let output = exec_git(&args)?;
        Ok(Object {
            repo: self,
            oid: String::from_utf8(output.stdout)?.trim().to_string(),
        })
    }

    // Non-standard method of getting a 'default' remote
    pub fn get_default_remote(&self) -> Result<Option<String>, GitAiError> {
        let remotes = self.remotes()?;
        if remotes.is_empty() {
            return Ok(None);
        }
        // Prefer 'origin' if it exists
        for i in 0..remotes.len() {
            if let Some(name) = remotes.get(i)
                && name == "origin"
            {
                return Ok(Some("origin".to_string()));
            }
        }
        // Otherwise, just use the first remote
        Ok(remotes.first().map(|s| s.to_string()))
    }

    #[allow(dead_code)]
    pub fn fetch_authorship(&self, remote_name: &str) -> Result<(), GitAiError> {
        // Discards whether notes were found or not, just returns success/error
        fetch_authorship_notes(self, remote_name).map(|_| ())
    }

    #[allow(dead_code)]
    pub fn push_authorship(&self, remote_name: &str) -> Result<(), GitAiError> {
        push_authorship_notes(self, remote_name)
    }

    pub fn upstream_remote(&self) -> Result<Option<String>, GitAiError> {
        // Get current branch name using exec_git
        let mut args = self.global_args_for_exec();
        args.push("branch".to_string());
        args.push("--show-current".to_string());
        let output = exec_git(&args)?;
        let branch = String::from_utf8(output.stdout)?.trim().to_string();
        if branch.is_empty() {
            return Ok(None);
        }
        let config_key = format!("branch.{}.remote", branch);
        self.config_get_str(&config_key)
    }

    pub fn resolve_author_spec(&self, author_spec: &str) -> Result<Option<String>, GitAiError> {
        // Use git rev-list to find the first commit by this author pattern
        let mut args = self.global_args_for_exec();
        args.push("rev-list".to_string());
        args.push("--all".to_string());
        args.push("-i".to_string());
        args.push("--max-count=1".to_string());
        args.push(format!("--author={}", author_spec));
        let output = match exec_git(&args) {
            Ok(output) => output,
            Err(GitAiError::GitCliError { code: Some(1), .. }) => {
                // No commit found
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let commit_oid = String::from_utf8(output.stdout)?.trim().to_string();
        if commit_oid.is_empty() {
            return Ok(None);
        }

        // Now get the author name/email from that commit
        let mut show_args = self.global_args_for_exec();
        show_args.push("show".to_string());
        show_args.push("-s".to_string());
        show_args.push("--format=%an <%ae>".to_string());
        show_args.push(commit_oid);
        let show_output = exec_git(&show_args)?;
        let author_line = String::from_utf8(show_output.stdout)?.trim().to_string();
        if author_line.is_empty() {
            Ok(None)
        } else {
            Ok(Some(author_line))
        }
    }

    // Create an iterator for the repo's references (git2-style)
    #[allow(dead_code)]
    pub fn references<'a>(&'a self) -> Result<References<'a>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("for-each-ref".to_string());
        args.push("--format=%(refname)".to_string());

        let output = exec_git(&args)?;
        let stdout = String::from_utf8(output.stdout)?;
        let refs: Vec<String> = stdout
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        Ok(References {
            repo: self,
            refs,
            index: 0,
        })
    }

    // Lookup a reference to one of the commits in a repository.
    pub fn find_commit(&self, oid: String) -> Result<Commit<'_>, GitAiError> {
        let typ = self.object_type(&oid)?;
        if typ != "commit" {
            return Err(GitAiError::Generic(format!(
                "Object is not a commit: {} (type: {})",
                oid, typ
            )));
        }
        Ok(Commit {
            repo: self,
            oid,
            authorship_log: std::cell::OnceCell::new(),
        })
    }

    // Lookup a reference to one of the objects in a repository.
    pub fn find_blob(&self, oid: String) -> Result<Blob<'_>, GitAiError> {
        let typ = self.object_type(&oid)?;
        if typ != "blob" {
            return Err(GitAiError::Generic(format!(
                "Object is not a blob: {} (type: {})",
                oid, typ
            )));
        }
        Ok(Blob { repo: self, oid })
    }

    // Lookup a reference to one of the objects in a repository.
    pub fn find_tree(&self, oid: String) -> Result<Tree<'_>, GitAiError> {
        let typ = self.object_type(&oid)?;
        if typ != "tree" {
            return Err(GitAiError::Generic(format!(
                "Object is not a tree: {} (type: {})",
                oid, typ
            )));
        }
        Ok(Tree { repo: self, oid })
    }

    /// Get the content of a file at a specific commit
    /// Uses `git show <commit>:<path>` for efficient single-call retrieval
    #[allow(dead_code)]
    pub fn get_file_content(
        &self,
        file_path: &str,
        commit_hash: &str,
    ) -> Result<Vec<u8>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("show".to_string());
        args.push(format!("{}:{}", commit_hash, file_path));
        let output = exec_git(&args)?;
        Ok(output.stdout)
    }

    /// Get content of all staged files concurrently
    /// Returns a HashMap of file paths to their staged content as strings
    /// Skips files that fail to read or aren't valid UTF-8
    pub fn get_all_staged_files_content(
        &self,
        file_paths: &[String],
    ) -> Result<HashMap<String, String>, GitAiError> {
        use futures::future::join_all;
        use std::sync::Arc;

        const MAX_CONCURRENT: usize = 30;

        let repo_global_args = self.global_args_for_exec();
        let semaphore = Arc::new(smol::lock::Semaphore::new(MAX_CONCURRENT));

        let futures: Vec<_> = file_paths
            .iter()
            .map(|file_path| {
                let mut args = repo_global_args.clone();
                args.push("show".to_string());
                args.push(format!(":{}", file_path));
                let file_path = file_path.clone();
                let semaphore = semaphore.clone();

                async move {
                    let _permit = semaphore.acquire().await;
                    let result = exec_git(&args).and_then(|output| {
                        String::from_utf8(output.stdout)
                            .map_err(|e| GitAiError::Utf8Error(e.utf8_error()))
                    });
                    (file_path, result)
                }
            })
            .collect();

        let results = smol::block_on(async { join_all(futures).await });

        let mut staged_files = HashMap::new();
        for (file_path, result) in results {
            if let Ok(content) = result {
                staged_files.insert(file_path, content);
            }
        }

        Ok(staged_files)
    }

    /// List all files changed in a commit
    /// Returns a HashSet of file paths relative to the repository root
    pub fn list_commit_files(
        &self,
        commit_sha: &str,
        pathspecs: Option<&HashSet<String>>,
    ) -> Result<HashSet<String>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("diff-tree".to_string());
        args.push("--no-commit-id".to_string());
        args.push("--name-only".to_string());
        args.push("-r".to_string());
        args.push("-z".to_string()); // NUL-separated output for proper UTF-8 handling

        // Find the commit to check if it has a parent
        let commit = self.find_commit(commit_sha.to_string())?;

        // For initial commits (no parent), compare against the empty tree
        if commit.parent_count()? == 0 {
            let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
            args.push(empty_tree.to_string());
        }

        args.push(commit_sha.to_string());

        // Add pathspecs if provided (only as CLI args when under threshold)
        let needs_post_filter = if let Some(paths) = pathspecs {
            // for case where pathspec filter provided BUT not pathspecs.
            // otherwise it would default to full repo
            if paths.is_empty() {
                return Ok(HashSet::new());
            }
            if paths.len() > MAX_PATHSPEC_ARGS {
                true
            } else {
                args.push("--".to_string());
                for path in paths {
                    args.push(path.clone());
                }
                false
            }
        } else {
            false
        };

        let output = exec_git(&args)?;

        // With -z, output is NUL-separated. The output may contain a trailing NUL.
        let mut files: HashSet<String> = output
            .stdout
            .split(|&b| b == 0)
            .filter(|bytes| !bytes.is_empty())
            .filter_map(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .collect();

        if needs_post_filter && let Some(paths) = pathspecs {
            files.retain(|path| paths.contains(path));
        }

        Ok(files)
    }

    /// Get added line ranges from git diff between two commits
    /// Returns a HashMap of file paths to vectors of added line numbers
    ///
    /// Uses `git diff -U0` to get unified diff with zero context lines,
    /// then parses the hunk headers to extract line numbers directly.
    /// This is much faster than fetching blobs and running TextDiff manually.
    pub fn diff_added_lines(
        &self,
        from_ref: &str,
        to_ref: &str,
        pathspecs: Option<&HashSet<String>>,
    ) -> Result<HashMap<String, Vec<u32>>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("diff".to_string());
        args.push("-U0".to_string()); // Zero context lines
        args.push("--no-color".to_string());
        args.push(from_ref.to_string());
        args.push(to_ref.to_string());

        // Add pathspecs if provided (only as CLI args when under threshold)
        let needs_post_filter = if let Some(paths) = pathspecs {
            // for case where pathspec filter provided BUT not pathspecs.
            // otherwise it would default to full repo
            if paths.is_empty() {
                return Ok(HashMap::new());
            }
            if paths.len() > MAX_PATHSPEC_ARGS {
                true
            } else {
                args.push("--".to_string());
                for path in paths {
                    args.push(path.clone());
                }
                false
            }
        } else {
            false
        };

        let output = exec_git(&args)?;
        let diff_output = String::from_utf8_lossy(&output.stdout);

        let mut result = parse_diff_added_lines(&diff_output)?;

        if needs_post_filter && let Some(paths) = pathspecs {
            result.retain(|path, _| paths.contains(path));
        }

        Ok(result)
    }

    /// Get list of changed files between two refs using `git diff --name-only`
    /// Returns a Vec of file paths that differ between the two refs
    pub fn diff_changed_files(
        &self,
        from_ref: &str,
        to_ref: &str,
    ) -> Result<Vec<String>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("diff".to_string());
        args.push("--name-only".to_string());
        args.push("-z".to_string()); // NUL-separated output for proper UTF-8 handling
        args.push(from_ref.to_string());
        args.push(to_ref.to_string());

        let output = exec_git(&args)?;

        // With -z, output is NUL-separated. The output may contain a trailing NUL.
        let files: Vec<String> = output
            .stdout
            .split(|&b| b == 0)
            .filter(|bytes| !bytes.is_empty())
            .filter_map(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .collect();

        Ok(files)
    }

    /// Get added line ranges from git diff between a commit and the working directory
    /// Returns a HashMap of file paths to vectors of added line numbers
    ///
    /// Similar to diff_added_lines but compares against the working directory
    #[allow(dead_code)]
    pub fn diff_workdir_added_lines(
        &self,
        from_ref: &str,
        pathspecs: Option<&HashSet<String>>,
    ) -> Result<HashMap<String, Vec<u32>>, GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("diff".to_string());
        args.push("-U0".to_string()); // Zero context lines
        args.push("--no-color".to_string());
        args.push(from_ref.to_string());

        // Add pathspecs if provided (only as CLI args when under threshold)
        let needs_post_filter = if let Some(paths) = pathspecs {
            // for case where pathspec filter provided BUT not pathspecs.
            // otherwise it would default to full repo
            if paths.is_empty() {
                return Ok(HashMap::new());
            }
            if paths.len() > MAX_PATHSPEC_ARGS {
                true
            } else {
                args.push("--".to_string());
                for path in paths {
                    args.push(path.clone());
                }
                false
            }
        } else {
            false
        };

        let output = exec_git(&args)?;
        let diff_output = String::from_utf8_lossy(&output.stdout);

        let mut result = parse_diff_added_lines(&diff_output)?;

        if needs_post_filter && let Some(paths) = pathspecs {
            result.retain(|path, _| paths.contains(path));
        }

        Ok(result)
    }

    /// Get added line ranges from git diff between a commit and the working directory,
    /// along with information about which lines are pure insertions (old_count=0).
    ///
    /// Returns (all_added_lines, pure_insertion_lines)
    /// Pure insertions are lines that were added without modifying existing lines at that position.
    #[allow(clippy::type_complexity)]
    pub fn diff_workdir_added_lines_with_insertions(
        &self,
        from_ref: &str,
        pathspecs: Option<&HashSet<String>>,
    ) -> Result<(HashMap<String, Vec<u32>>, HashMap<String, Vec<u32>>), GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("diff".to_string());
        args.push("-U0".to_string()); // Zero context lines
        args.push("--no-color".to_string());
        args.push(from_ref.to_string());

        // Add pathspecs if provided (only as CLI args when under threshold)
        let needs_post_filter = if let Some(paths) = pathspecs {
            // for case where pathspec filter provided BUT not pathspecs.
            // otherwise it would default to full repo
            if paths.is_empty() {
                return Ok((HashMap::new(), HashMap::new()));
            }
            if paths.len() > MAX_PATHSPEC_ARGS {
                true
            } else {
                args.push("--".to_string());
                for path in paths {
                    args.push(path.clone());
                }
                false
            }
        } else {
            false
        };

        let output = exec_git(&args)?;
        let diff_output = String::from_utf8_lossy(&output.stdout);

        let (mut all_added, mut pure_insertions) =
            parse_diff_added_lines_with_insertions(&diff_output)?;

        if needs_post_filter && let Some(paths) = pathspecs {
            all_added.retain(|path, _| paths.contains(path));
            pure_insertions.retain(|path, _| paths.contains(path));
        }

        Ok((all_added, pure_insertions))
    }

    pub fn fetch_branch(&self, branch_name: &str, remote_name: &str) -> Result<(), GitAiError> {
        let mut args = self.global_args_for_exec();
        args.push("fetch".to_string());
        args.push(remote_name.to_string());
        args.push(branch_name.to_string());
        exec_git(&args)?;
        Ok(())
    }
}

pub fn find_repository(global_args: &[String]) -> Result<Repository, GitAiError> {
    let mut rev_parse_args = global_args.to_owned();
    rev_parse_args.push("rev-parse".to_string());
    // Use --git-dir instead of --absolute-git-dir for compatibility with Git < 2.13
    // (--absolute-git-dir was added in Git 2.13; older versions output the literal
    // string "absolute-git-dir" instead of the resolved path).
    rev_parse_args.push("--is-bare-repository".to_string());
    rev_parse_args.push("--git-dir".to_string());

    let rev_parse_output = exec_git(&rev_parse_args)?;
    let rev_parse_stdout = String::from_utf8(rev_parse_output.stdout)?;
    let mut lines = rev_parse_stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());

    let is_bare = match lines.next() {
        Some("true") => true,
        Some("false") => false,
        Some(other) => {
            return Err(GitAiError::Generic(format!(
                "Unexpected --is-bare-repository output: {}",
                other
            )));
        }
        None => {
            return Err(GitAiError::Generic(
                "Missing --is-bare-repository output from git rev-parse".to_string(),
            ));
        }
    };

    let git_dir_str = lines.next().ok_or_else(|| {
        GitAiError::Generic("Missing --git-dir output from git rev-parse".to_string())
    })?;
    let command_base_dir = resolve_command_base_dir(global_args)?;
    let git_dir = if Path::new(git_dir_str).is_relative() {
        command_base_dir.join(git_dir_str)
    } else {
        PathBuf::from(git_dir_str)
    };

    if !git_dir.is_dir() {
        return Err(GitAiError::Generic(format!(
            "Git directory does not exist: {}",
            git_dir.display()
        )));
    }

    let workdir = if is_bare {
        git_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
            GitAiError::Generic(format!(
                "Git directory has no parent: {}",
                git_dir.display()
            ))
        })?
    } else {
        let mut top_level_args = global_args.to_owned();
        top_level_args.push("rev-parse".to_string());
        top_level_args.push("--show-toplevel".to_string());
        let output = exec_git(&top_level_args)?;
        PathBuf::from(String::from_utf8(output.stdout)?.trim())
    };

    if !workdir.is_dir() {
        return Err(GitAiError::Generic(format!(
            "Work directory does not exist: {}",
            workdir.display()
        )));
    }

    // Ensure all internal git commands use a stable repository root consistently.
    let mut normalized_global_args = global_args.to_owned();
    let command_root = if is_bare {
        git_dir.display().to_string()
    } else {
        workdir.display().to_string()
    };

    if normalized_global_args.is_empty() {
        normalized_global_args = vec!["-C".to_string(), command_root];
    } else if normalized_global_args.len() == 2
        && normalized_global_args[0] == "-C"
        && normalized_global_args[1] != command_root
    {
        normalized_global_args[1] = command_root;
    }

    // Canonicalize workdir for reliable path comparisons (especially on Windows)
    // On Windows, canonical paths use the \\?\ UNC prefix, which makes path.starts_with()
    // comparisons work correctly. We store both regular and canonical versions.
    let canonical_workdir = workdir.canonicalize().map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to canonicalize working directory {}: {}",
            workdir.display(),
            e
        ))
    })?;

    Ok(Repository {
        global_args: normalized_global_args,
        storage: RepoStorage::for_repo_path(&git_dir, &workdir),
        git_dir,
        pre_command_base_commit: None,
        pre_command_refname: None,
        pre_reset_target_commit: None,
        workdir,
        canonical_workdir,
    })
}

fn resolve_command_base_dir(global_args: &[String]) -> Result<PathBuf, GitAiError> {
    let mut base = std::env::current_dir().map_err(GitAiError::IoError)?;
    let mut idx = 0usize;

    while idx < global_args.len() {
        if global_args[idx] == "-C" {
            let path_arg = global_args.get(idx + 1).ok_or_else(|| {
                GitAiError::Generic("Missing path after -C in global git args".to_string())
            })?;

            let next_base = PathBuf::from(path_arg);
            base = if next_base.is_absolute() {
                next_base
            } else {
                base.join(next_base)
            };
            idx += 2;
            continue;
        }
        idx += 1;
    }

    Ok(base)
}

#[allow(dead_code)]
pub fn from_bare_repository(git_dir: &Path) -> Result<Repository, GitAiError> {
    let workdir = git_dir
        .parent()
        .ok_or_else(|| GitAiError::Generic("Git directory has no parent".to_string()))?
        .to_path_buf();
    let global_args = vec!["-C".to_string(), git_dir.to_string_lossy().to_string()];

    let canonical_workdir = workdir.canonicalize().unwrap_or_else(|_| workdir.clone());

    Ok(Repository {
        global_args,
        storage: RepoStorage::for_repo_path(git_dir, &workdir),
        git_dir: git_dir.to_path_buf(),
        pre_command_base_commit: None,
        pre_command_refname: None,
        pre_reset_target_commit: None,
        workdir,
        canonical_workdir,
    })
}

pub fn find_repository_in_path(path: &str) -> Result<Repository, GitAiError> {
    let global_args = vec!["-C".to_string(), path.to_string()];
    find_repository(&global_args)
}

/// Find the git repository that contains the given file path by walking up the directory tree.
///
/// This function is useful when working with multi-repository workspaces where the workspace
/// root itself may not be a git repository, but contains multiple independent git repositories.
///
/// # Arguments
///  * `file_path` - Absolute path to a file
///  * `workspace_root` - Optional workspace root path. If provided, the search will stop at this
///    boundary to avoid finding repositories outside the workspace.
///
/// # Returns
/// * `Ok(Repository)` - The repository containing the file
/// * `Err(GitAiError)` - If no repository is found or other errors occur
pub fn find_repository_for_file(
    file_path: &str,
    workspace_root: Option<&str>,
) -> Result<Repository, GitAiError> {
    let file_path = PathBuf::from(file_path);

    // Get the directory containing the file (or the path itself if it's a directory)
    let start_dir = if file_path.is_dir() {
        file_path.clone()
    } else {
        file_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| file_path.clone())
    };

    // Canonicalize paths for consistent comparison
    let start_dir = start_dir
        .canonicalize()
        .unwrap_or_else(|_| start_dir.clone());

    let workspace_boundary = workspace_root.map(|root| {
        PathBuf::from(root)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(root))
    });

    // Walk up the directory tree looking for a .git directory
    let mut current_dir = Some(start_dir.as_path());

    while let Some(dir) = current_dir {
        // Check if we've reached the workspace boundary
        if let Some(ref boundary) = workspace_boundary {
            // Stop if we've gone above the workspace root
            if !dir.starts_with(boundary) && dir != boundary.as_path() {
                break;
            }
        }

        // Check for .git directory or file (file for submodules/worktrees)
        let git_path = dir.join(".git");
        if git_path.exists() {
            // Found a .git - but we need to check if this is a submodule
            // Submodules have a .git file (not directory) that points to the parent's .git/modules
            if git_path.is_file() {
                // This is a submodule - read the file to check if it points to modules/
                if let Ok(content) = std::fs::read_to_string(&git_path)
                    && content.contains("gitdir:")
                    && content.contains("/modules/")
                {
                    // This is a submodule, skip it and continue searching up
                    current_dir = dir.parent();
                    continue;
                }
            }

            // Found a real git repository, use find_repository_in_path
            return find_repository_in_path(&dir.to_string_lossy());
        }

        current_dir = dir.parent();
    }

    Err(GitAiError::Generic(format!(
        "No git repository found for file: {}",
        file_path.display()
    )))
}

/// Group edited file paths by their containing git repository.
///
/// This function takes a list of file paths and groups them by the git repository
/// they belong to. Files that don't belong to any repository are collected separately.
///
/// # Arguments
/// * `file_paths` - List of absolute file paths to group
/// * `workspace_root` - Optional workspace root to limit repository detection
///
/// # Returns
/// A tuple of:
/// * `HashMap<PathBuf, (Repository, Vec<String>)>` - Map of repo root to (repo, file paths)
/// * `Vec<String>` - Files that couldn't be associated with any repository
#[allow(clippy::type_complexity)]
pub fn group_files_by_repository(
    file_paths: &[String],
    workspace_root: Option<&str>,
) -> (HashMap<PathBuf, (Repository, Vec<String>)>, Vec<String>) {
    let mut repo_files: HashMap<PathBuf, (Repository, Vec<String>)> = HashMap::new();
    let mut orphan_files: Vec<String> = Vec::new();

    for file_path in file_paths {
        match find_repository_for_file(file_path, workspace_root) {
            Ok(repo) => {
                let workdir = match repo.workdir() {
                    Ok(dir) => dir,
                    Err(_) => {
                        orphan_files.push(file_path.clone());
                        continue;
                    }
                };

                repo_files
                    .entry(workdir.clone())
                    .or_insert_with(|| (repo, Vec::new()))
                    .1
                    .push(file_path.clone());
            }
            Err(_) => {
                orphan_files.push(file_path.clone());
            }
        }
    }

    (repo_files, orphan_files)
}

/// Helper to execute a git command
pub fn exec_git(args: &[String]) -> Result<Output, GitAiError> {
    // TODO Make sure to handle process signals, etc.
    let mut cmd = Command::new(config::Config::get().git_cmd());
    cmd.args(args);

    #[cfg(windows)]
    {
        if !is_interactive_terminal() {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
    }

    let output = cmd.output().map_err(GitAiError::IoError)?;

    if !output.status.success() {
        let code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(GitAiError::GitCliError {
            code,
            stderr,
            args: args.to_vec(),
        });
    }

    Ok(output)
}

/// Helper to execute a git command with data provided on stdin
pub fn exec_git_stdin(args: &[String], stdin_data: &[u8]) -> Result<Output, GitAiError> {
    // TODO Make sure to handle process signals, etc.
    let mut cmd = Command::new(config::Config::get().git_cmd());
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    {
        if !is_interactive_terminal() {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
    }

    let mut child = cmd.spawn().map_err(GitAiError::IoError)?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(e) = stdin.write_all(stdin_data) {
            return Err(GitAiError::IoError(e));
        }
    }

    let output = child.wait_with_output().map_err(GitAiError::IoError)?;

    if !output.status.success() {
        let code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(GitAiError::GitCliError {
            code,
            stderr,
            args: args.to_vec(),
        });
    }

    Ok(output)
}

/// Helper to execute a git command with data provided on stdin and additional environment variables
#[allow(dead_code)]
pub fn exec_git_stdin_with_env(
    args: &[String],
    env: &[(String, String)],
    stdin_data: &[u8],
) -> Result<Output, GitAiError> {
    // TODO Make sure to handle process signals, etc.
    let mut cmd = Command::new(config::Config::get().git_cmd());
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Apply env overrides
    for (k, v) in env.iter() {
        cmd.env(k, v);
    }

    #[cfg(windows)]
    {
        if !is_interactive_terminal() {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
    }

    let mut child = cmd.spawn().map_err(GitAiError::IoError)?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(e) = stdin.write_all(stdin_data) {
            return Err(GitAiError::IoError(e));
        }
    }

    let output = child.wait_with_output().map_err(GitAiError::IoError)?;

    if !output.status.success() {
        let code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(GitAiError::GitCliError {
            code,
            stderr,
            args: args.to_vec(),
        });
    }

    Ok(output)
}

/// Parse git version string (e.g., "git version 2.39.3 (Apple Git-146)") to extract major, minor, patch.
/// Returns None if the version cannot be parsed.
fn parse_git_version(version_str: &str) -> Option<(u32, u32, u32)> {
    // Expected format: "git version X.Y.Z" or "git version X.Y.Z.windows.N" etc.
    let version_str = version_str.trim();
    let parts: Vec<&str> = version_str.split_whitespace().collect();

    // Find the version number part (usually the 3rd element)
    let version_part = parts.get(2)?;

    // Parse version like "2.39.3" or "2.39.3.windows.1"
    let version_nums: Vec<&str> = version_part.split('.').collect();
    if version_nums.len() < 2 {
        return None;
    }

    let major = version_nums.first()?.parse::<u32>().ok()?;
    let minor = version_nums.get(1)?.parse::<u32>().ok()?;
    let patch = version_nums
        .get(2)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    Some((major, minor, patch))
}

/// Parse git diff output to extract added line numbers per file
///
/// Parses unified diff format hunk headers like:
/// @@ -10,2 +15,5 @@
///
/// This means: old file line 10 (2 lines), new file line 15 (5 lines)
/// We extract the "new file" line numbers to know which lines were added.
fn parse_diff_added_lines(diff_output: &str) -> Result<HashMap<String, Vec<u32>>, GitAiError> {
    let mut result: HashMap<String, Vec<u32>> = HashMap::new();
    let mut current_file: Option<String> = None;

    for line in diff_output.lines() {
        // Track current file being diffed
        // Git outputs paths in two formats:
        // 1. Unquoted: +++ b/path/to/file.txt (or w/ for workdir diffs)
        // 2. Quoted (for non-ASCII): +++ "b/path/to/file.txt" (with octal escapes inside)
        if let Some(raw_path) = line.strip_prefix("+++ b/") {
            // Unquoted path (ASCII only)
            // Note: Git adds trailing tab after filenames with spaces, so we trim_end
            let file_path = crate::utils::unescape_git_path(raw_path.trim_end());
            current_file = Some(file_path);
        } else if let Some(raw_path) = line.strip_prefix("+++ w/") {
            // Workdir diff uses w/ prefix instead of b/
            let file_path = crate::utils::unescape_git_path(raw_path.trim_end());
            current_file = Some(file_path);
        } else if line.starts_with("+++ \"") {
            // Quoted path (non-ASCII chars) - unescape the entire quoted portion after "+++ "
            if let Some(quoted_suffix) = line.strip_prefix("+++ ") {
                let unescaped = crate::utils::unescape_git_path(quoted_suffix);
                // Strip the prefix (b/ or w/) after unescaping
                let file_path = if let Some(stripped) = unescaped
                    .strip_prefix("b/")
                    .or(unescaped.strip_prefix("w/"))
                {
                    stripped.to_string()
                } else {
                    unescaped
                };
                current_file = Some(file_path);
            }
        } else if line.starts_with("+++ /dev/null") {
            // File was deleted
            current_file = None;
        } else if line.starts_with("@@ ") {
            // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
            if let Some(ref file) = current_file
                && let Some((added_lines, _is_pure_insertion)) = parse_hunk_header(line)
            {
                result.entry(file.clone()).or_default().extend(added_lines);
            }
        }
    }

    // Sort and deduplicate line numbers for each file
    for lines in result.values_mut() {
        lines.sort_unstable();
        lines.dedup();
    }

    Ok(result)
}

/// Parses the unified diff output to extract line numbers of added lines,
/// along with information about which are pure insertions (old_count=0).
///
/// Returns (all_added_lines, pure_insertion_lines)
#[allow(clippy::type_complexity)]
fn parse_diff_added_lines_with_insertions(
    diff_output: &str,
) -> Result<(HashMap<String, Vec<u32>>, HashMap<String, Vec<u32>>), GitAiError> {
    let mut all_lines: HashMap<String, Vec<u32>> = HashMap::new();
    let mut insertion_lines: HashMap<String, Vec<u32>> = HashMap::new();
    let mut current_file: Option<String> = None;

    for line in diff_output.lines() {
        // Track current file being diffed
        // Git outputs paths in two formats:
        // 1. Unquoted: +++ b/path/to/file.txt (or w/ for workdir diffs)
        // 2. Quoted (for non-ASCII): +++ "b/path/to/file.txt" (with octal escapes inside)
        if let Some(raw_path) = line.strip_prefix("+++ b/") {
            // Unquoted path (ASCII only)
            // Note: Git adds trailing tab after filenames with spaces, so we trim_end
            let file_path = crate::utils::unescape_git_path(raw_path.trim_end());
            current_file = Some(file_path);
        } else if let Some(raw_path) = line.strip_prefix("+++ w/") {
            // Workdir diff uses w/ prefix instead of b/
            let file_path = crate::utils::unescape_git_path(raw_path.trim_end());
            current_file = Some(file_path);
        } else if line.starts_with("+++ \"") {
            // Quoted path (non-ASCII chars) - unescape the entire quoted portion after "+++ "
            if let Some(quoted_suffix) = line.strip_prefix("+++ ") {
                let unescaped = crate::utils::unescape_git_path(quoted_suffix);
                // Strip the prefix (b/ or w/) after unescaping
                let file_path = if let Some(stripped) = unescaped
                    .strip_prefix("b/")
                    .or(unescaped.strip_prefix("w/"))
                {
                    stripped.to_string()
                } else {
                    unescaped
                };
                current_file = Some(file_path);
            }
        } else if line.starts_with("+++ /dev/null") {
            // File was deleted
            current_file = None;
        } else if line.starts_with("@@ ") {
            // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
            if let Some(ref file) = current_file
                && let Some((added_lines, is_pure_insertion)) = parse_hunk_header(line)
            {
                all_lines
                    .entry(file.clone())
                    .or_default()
                    .extend(added_lines.clone());

                if is_pure_insertion {
                    insertion_lines
                        .entry(file.clone())
                        .or_default()
                        .extend(added_lines);
                }
            }
        }
    }

    // Sort and deduplicate line numbers for each file
    for lines in all_lines.values_mut() {
        lines.sort_unstable();
        lines.dedup();
    }
    for lines in insertion_lines.values_mut() {
        lines.sort_unstable();
        lines.dedup();
    }

    Ok((all_lines, insertion_lines))
}

/// Parse a hunk header line to extract added line numbers and whether it's a pure insertion
///
/// Format: @@ -old_start,old_count +new_start,new_count @@
/// Returns (line numbers that were added, is_pure_insertion)
/// is_pure_insertion is true when old_count=0, meaning these are new lines, not modifications
fn parse_hunk_header(line: &str) -> Option<(Vec<u32>, bool)> {
    // Find the part between @@ and @@
    let parts: Vec<&str> = line.split("@@").collect();
    if parts.len() < 2 {
        return None;
    }

    let hunk_info = parts[1].trim();

    // Split by space to get old and new ranges
    let ranges: Vec<&str> = hunk_info.split_whitespace().collect();
    if ranges.len() < 2 {
        return None;
    }

    // Parse the old file range (starts with '-')
    let old_range = ranges
        .iter()
        .find(|r| r.starts_with('-'))?
        .trim_start_matches('-');

    // Parse "start,count" or just "start" for old range
    let old_parts: Vec<&str> = old_range.split(',').collect();
    let old_count: u32 = if old_parts.len() > 1 {
        old_parts[1].parse().ok()?
    } else {
        1 // If no count specified, it's 1 line
    };

    // Parse the new file range (starts with '+')
    let new_range = ranges
        .iter()
        .find(|r| r.starts_with('+'))?
        .trim_start_matches('+');

    // Parse "start,count" or just "start"
    let new_parts: Vec<&str> = new_range.split(',').collect();
    let start: u32 = new_parts[0].parse().ok()?;
    let count: u32 = if new_parts.len() > 1 {
        new_parts[1].parse().ok()?
    } else {
        1 // If no count specified, it's 1 line
    };

    // If count is 0, no lines were added (only deleted)
    if count == 0 {
        return Some((Vec::new(), false));
    }

    // Generate all line numbers in the range
    let lines: Vec<u32> = (start..start + count).collect();

    // Pure insertion if old_count is 0 (no lines from old file were modified)
    let is_pure_insertion = old_count == 0;

    Some((lines, is_pure_insertion))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_parse_git_version_standard() {
        // Standard git version format
        assert_eq!(parse_git_version("git version 2.39.3"), Some((2, 39, 3)));
        assert_eq!(parse_git_version("git version 2.23.0"), Some((2, 23, 0)));
        assert_eq!(parse_git_version("git version 1.8.5"), Some((1, 8, 5)));
    }

    #[test]
    fn test_parse_git_version_apple_git() {
        // macOS Apple Git format
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39, 3))
        );
    }

    #[test]
    fn test_parse_git_version_windows() {
        // Windows git format
        assert_eq!(
            parse_git_version("git version 2.42.0.windows.2"),
            Some((2, 42, 0))
        );
    }

    #[test]
    fn test_parse_git_version_no_patch() {
        // Version without patch number
        assert_eq!(parse_git_version("git version 2.39"), Some((2, 39, 0)));
    }

    #[test]
    fn test_parse_git_version_with_newline() {
        // Version string with trailing newline
        assert_eq!(parse_git_version("git version 2.39.3\n"), Some((2, 39, 3)));
    }

    #[test]
    fn test_parse_git_version_invalid() {
        // Invalid formats should return None
        assert_eq!(parse_git_version(""), None);
        assert_eq!(parse_git_version("not a version"), None);
        assert_eq!(parse_git_version("git version"), None);
        assert_eq!(parse_git_version("git version x.y.z"), None);
    }

    #[test]
    fn test_list_commit_files_with_utf8_filename() {
        use crate::git::test_utils::TmpRepo;

        // Create a test repo with a UTF-8 filename
        let tmp_repo = TmpRepo::new().unwrap();

        // Write a file with Chinese characters in its name
        let chinese_filename = "中文文件.txt";
        tmp_repo
            .write_file(chinese_filename, "Hello, 世界!\n", false)
            .unwrap();

        // Create an initial commit (using trigger_checkpoint_with_author for human checkpoint)
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        let _authorship_log = tmp_repo.commit_with_message("Add Chinese file").unwrap();

        // Now get the commit SHA using git-ai repository methods
        let repo = tmp_repo.gitai_repo();
        let head = repo.head().unwrap();
        let commit_sha = head.target().unwrap();

        // Test list_commit_files
        let files = repo.list_commit_files(&commit_sha, None).unwrap();

        // Debug: print what we got
        println!("Files in commit: {:?}", files);

        // The file should be in the list with its UTF-8 name
        assert!(
            files.contains(chinese_filename),
            "Should contain the Chinese filename '{}', but got: {:?}",
            chinese_filename,
            files
        );
    }

    #[test]
    fn test_parse_diff_added_lines_with_insertions_standard_prefix() {
        // Test diff with standard b/ prefix (commit-to-commit diff)
        let diff = r#"diff --git a/test.txt b/test.txt
index 0000000..abc1234 100644
--- a/test.txt
+++ b/test.txt
@@ -0,0 +1,2 @@
+line 1
+line 2"#;

        let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
        assert_eq!(added_lines.get("test.txt"), Some(&vec![1, 2]));
        assert_eq!(insertion_lines.get("test.txt"), Some(&vec![1, 2]));
    }

    #[test]
    fn test_parse_diff_added_lines_with_insertions_workdir_prefix() {
        // Test diff with w/ prefix (commit-to-workdir diff)
        let diff = r#"diff --git c/test.txt w/test.txt
index a751413..8adaa6c 100644
--- c/test.txt
+++ w/test.txt
@@ -0,0 +1,2 @@
+// AI added line 1
+// AI added line 2"#;

        let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
        assert_eq!(added_lines.get("test.txt"), Some(&vec![1, 2]));
        assert_eq!(insertion_lines.get("test.txt"), Some(&vec![1, 2]));
    }

    #[test]
    fn test_parse_diff_added_lines_with_insertions_quoted_paths() {
        // Test diff with quoted paths containing spaces
        let diff = r#"diff --git "a/my file.txt" "b/my file.txt"
index 0000000..abc1234 100644
--- "a/my file.txt"
+++ "b/my file.txt"
@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3"#;

        let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
        assert_eq!(added_lines.get("my file.txt"), Some(&vec![1, 2, 3]));
        assert_eq!(insertion_lines.get("my file.txt"), Some(&vec![1, 2, 3]));
    }

    #[test]
    fn test_parse_diff_added_lines_with_insertions_quoted_workdir_paths() {
        // Test diff with quoted w/ paths
        let diff = r#"diff --git "c/my file.txt" "w/my file.txt"
index 0000000..abc1234 100644
--- "c/my file.txt"
+++ "w/my file.txt"
@@ -0,0 +1,2 @@
+line 1
+line 2"#;

        let (added_lines, insertion_lines) = parse_diff_added_lines_with_insertions(diff).unwrap();
        assert_eq!(added_lines.get("my file.txt"), Some(&vec![1, 2]));
        assert_eq!(insertion_lines.get("my file.txt"), Some(&vec![1, 2]));
    }

    #[test]
    fn find_repository_in_path_supports_bare_repositories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let bare = temp.path().join("repo.git");
        fs::create_dir_all(&source).expect("create source");

        run_git(&source, &["init"]);
        run_git(&source, &["config", "user.name", "Test User"]);
        run_git(&source, &["config", "user.email", "test@example.com"]);
        fs::write(source.join("README.md"), "# repo\n").expect("write readme");
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-m", "initial"]);
        run_git(
            temp.path(),
            &[
                "clone",
                "--bare",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );

        let repo = find_repository_in_path(bare.to_str().unwrap()).expect("find bare repo");
        assert!(repo.is_bare_repository().expect("bare check"));
        assert_eq!(
            repo.path().canonicalize().expect("canonical bare"),
            bare.canonicalize().expect("canonical path")
        );
    }

    #[test]
    fn find_repository_in_path_bare_repo_can_read_head_gitattributes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let bare = temp.path().join("repo.git");
        fs::create_dir_all(&source).expect("create source");

        run_git(&source, &["init"]);
        run_git(&source, &["config", "user.name", "Test User"]);
        run_git(&source, &["config", "user.email", "test@example.com"]);
        fs::write(
            source.join(".gitattributes"),
            "generated/** linguist-generated=true\n",
        )
        .expect("write attrs");
        fs::write(source.join("README.md"), "# repo\n").expect("write readme");
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-m", "initial"]);
        run_git(
            temp.path(),
            &[
                "clone",
                "--bare",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );

        let repo = find_repository_in_path(bare.to_str().unwrap()).expect("find bare repo");
        let content = repo
            .get_file_content(".gitattributes", "HEAD")
            .expect("read attrs from HEAD");
        let content = String::from_utf8(content).expect("utf8 attrs");
        assert!(content.contains("generated/** linguist-generated=true"));
    }
}
