use crate::authorship::attribution_tracker::Attribution;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::post_commit::post_commit;
use crate::authorship::working_log::{Checkpoint, CheckpointKind};
use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::commands::{blame, checkpoint::run as checkpoint};
use crate::error::GitAiError;
use crate::git::repository::Repository as GitAiRepository;
use git2::{Repository, Signature};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

// Create a guaranteed-unique temporary directory under the OS temp dir.
// Combines high-resolution time, process id, and an atomic counter, retrying on collisions.
fn create_unique_tmp_dir(prefix: &str) -> Result<PathBuf, GitAiError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();

    // Try a handful of times in the extremely unlikely case of collision
    for _attempt in 0..100u32 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir_name = format!("{}-{}-{}-{}", prefix, now, pid, seq);
        let path = base.join(dir_name);

        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(GitAiError::IoError(e)),
        }
    }

    Err(GitAiError::Generic(
        "Failed to create a unique temporary directory after multiple attempts".to_string(),
    ))
}

#[allow(dead_code)]
pub struct TmpFile {
    repo: TmpRepo,
    filename: String,
    contents: String,
}

#[allow(dead_code)]
impl TmpFile {
    /// Updates the entire contents of the file
    pub fn update(&mut self, new_contents: &str) -> Result<(), GitAiError> {
        self.contents = new_contents.to_string();
        self.write_to_disk()?;
        self.flush_to_disk()
    }

    /// Appends content to the end of the file
    pub fn append(&mut self, content: &str) -> Result<(), GitAiError> {
        // Refresh from disk first – the file may have changed due to a branch checkout
        if let Ok(disk_contents) = fs::read_to_string(self.repo.path.join(&self.filename)) {
            self.contents = disk_contents;
        }

        // Guarantee we have a newline separator before appending (but not for empty files)
        if !self.contents.is_empty() && !self.contents.ends_with('\n') {
            self.contents.push('\n');
        }

        self.contents.push_str(content);
        self.write_to_disk()?;
        self.flush_to_disk()
    }

    /// Prepends content to the beginning of the file
    pub fn prepend(&mut self, content: &str) -> Result<(), GitAiError> {
        // Refresh from disk first – the file may have changed due to a branch checkout
        if let Ok(disk_contents) = fs::read_to_string(self.repo.path.join(&self.filename)) {
            self.contents = disk_contents;
        }

        // Create new content with prepended text
        let mut new_contents = content.to_string();

        // Add a newline separator if the prepended content doesn't end with one
        if !content.ends_with('\n') {
            new_contents.push('\n');
        }

        // Add the original content
        new_contents.push_str(&self.contents);

        self.contents = new_contents;
        self.write_to_disk()?;
        self.flush_to_disk()
    }

    /// Inserts content at a specific position
    pub fn insert_at(&mut self, position: usize, content: &str) -> Result<(), GitAiError> {
        if position > self.contents.len() {
            return Err(GitAiError::Generic(format!(
                "Position {} is out of bounds for file with {} characters",
                position,
                self.contents.len()
            )));
        }

        let mut new_contents = String::new();
        new_contents.push_str(&self.contents[..position]);
        new_contents.push_str(content);
        new_contents.push_str(&self.contents[position..]);

        self.contents = new_contents;
        self.write_to_disk()?;
        self.flush_to_disk()
    }

    /// Replaces content at a specific position with new content
    pub fn replace_at(&mut self, position: usize, new_content: &str) -> Result<(), GitAiError> {
        if position > self.contents.len() {
            return Err(GitAiError::Generic(format!(
                "Position {} is out of bounds for file with {} characters",
                position,
                self.contents.len()
            )));
        }
        let mut new_contents = self.contents.clone();
        new_contents.replace_range(position..position + new_content.len(), new_content);
        self.contents = new_contents;
        self.write_to_disk()?;
        self.flush_to_disk()
    }

    /// Replaces a range of lines with new content
    pub fn replace_range(
        &mut self,
        start_line: usize,
        end_line: usize,
        new_content: &str,
    ) -> Result<(), GitAiError> {
        // Refresh from disk first to stay in sync with the current branch version
        if let Ok(disk_contents) = fs::read_to_string(self.repo.path.join(&self.filename)) {
            self.contents = disk_contents;
        }

        let file_lines = self.contents.lines().collect::<Vec<&str>>();

        if start_line > file_lines.len()
            || end_line > file_lines.len() + 1
            || start_line >= end_line
        {
            return Err(GitAiError::Generic(format!(
                "Invalid line range [{}, {}) for file with {} lines",
                start_line,
                end_line,
                file_lines.len()
            )));
        }

        let mut new_contents = String::new();

        // Add lines before the range (1-indexed to 0-indexed conversion)
        for line in file_lines[..(start_line - 1)].iter() {
            new_contents.push_str(line);
            new_contents.push('\n');
        }

        // Add the new content (split into lines and add each line)
        for line in new_content.lines() {
            new_contents.push_str(line);
            new_contents.push('\n');
        }

        // Add lines after the range (1-indexed to 0-indexed conversion)
        // end_line is exclusive and 1-indexed, so we convert to 0-indexed: (end_line - 1)
        // But since it's exclusive, we actually want the line AT end_line (1-indexed), which is at index (end_line - 1)
        // Wait, if end_line is exclusive, we want lines starting from end_line (1-indexed) = index (end_line - 1)
        if end_line - 1 < file_lines.len() {
            for line in file_lines[(end_line - 1)..].iter() {
                new_contents.push_str(line);
                new_contents.push('\n');
            }
        }

        // Remove trailing newline if the original didn't have one
        if !self.contents.ends_with('\n') && !new_contents.is_empty() {
            new_contents.pop();
        }

        self.contents = new_contents;
        self.write_to_disk()?;
        self.flush_to_disk()
    }

    /// Gets the current contents of the file
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// Gets the filename
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Gets the full path of the file
    pub fn path(&self) -> PathBuf {
        self.repo.path.join(&self.filename)
    }

    /// Gets the length of the file contents
    pub fn len(&self) -> usize {
        self.contents.len()
    }

    /// Checks if the file is empty
    pub fn is_empty(&self) -> bool {
        self.contents.is_empty()
    }

    /// Clears all contents from the file
    pub fn clear(&mut self) -> Result<(), GitAiError> {
        self.contents.clear();
        self.write_to_disk()?;
        self.flush_to_disk()
    }

    /// Writes the current contents to disk
    fn write_to_disk(&self) -> Result<(), GitAiError> {
        let file_path = self.repo.path.join(&self.filename);

        // Create parent directories if they don't exist
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write the file
        fs::write(&file_path, &self.contents)?;

        // Add to git index using the filename directly
        let mut index = self.repo.repo_git2.index()?;
        index.add_path(std::path::Path::new(&self.filename))?;
        index.write()?;

        Ok(())
    }

    /// Flushes the file to disk to ensure all changes are written
    fn flush_to_disk(&self) -> Result<(), GitAiError> {
        use std::fs::OpenOptions;
        use std::io::Write;
        let file_path = self.repo.path.join(&self.filename);
        if let Ok(mut file) = OpenOptions::new().write(true).open(&file_path) {
            file.flush()?;
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub struct TmpRepo {
    path: PathBuf,
    repo_git2: Repository,
    repo_gitai: GitAiRepository,
}

#[allow(dead_code)]
impl TmpRepo {
    /// Creates a new temporary repository with a randomly generated directory
    pub fn new() -> Result<Self, GitAiError> {
        // Set test database path if not already set (for in-process unit tests)
        // OnceLock means DB is initialized once per process, so all unit tests
        // in this process will share this test DB - but won't touch production DB
        if std::env::var("GIT_AI_TEST_DB_PATH").is_err() {
            let test_db_path = std::env::temp_dir().join("git-ai-unit-test-db");
            // SAFETY: This is only called in test code, and we're setting a test-specific
            // env var before any threads access the database. The OnceLock pattern ensures
            // the database path is read only once, so this is safe.
            unsafe {
                std::env::set_var("GIT_AI_TEST_DB_PATH", &test_db_path);
            }
        }

        // Generate a robust, unique temporary directory path
        let tmp_dir = create_unique_tmp_dir("git-ai-tmp")?;

        println!("tmp_dir: {:?}", tmp_dir);

        // Initialize git repository
        let repo_git2 = Repository::init(&tmp_dir)?;

        // Initialize gitai repository
        let repo_gitai =
            crate::git::repository::find_repository_in_path(tmp_dir.to_str().unwrap())?;

        // Configure git user for commits
        let mut config = repo_git2.config()?;
        config.set_str("user.name", "Test User")?;
        config.set_str("user.email", "test@example.com")?;

        // (No initial empty commit)
        Ok(TmpRepo {
            path: tmp_dir,
            repo_git2,
            repo_gitai,
        })
    }

    pub fn new_with_base_commit() -> Result<(Self, TmpFile, TmpFile), GitAiError> {
        let repo = TmpRepo::new()?;
        let lines_file = repo.write_file("lines.md", LINES, true)?;
        let alphabet_file = repo.write_file("alphabet.md", ALPHABET, true)?;
        repo.trigger_checkpoint_with_author("test_user")?;
        repo.commit_with_message("initial commit")?;
        Ok((repo, lines_file, alphabet_file))
    }

    /// Writes a file with the given filename and contents, returns a TmpFile for further updates
    pub fn write_file(
        &self,
        filename: &str,
        contents: &str,
        add_to_git: bool,
    ) -> Result<TmpFile, GitAiError> {
        let file_path = self.path.join(filename);

        // Create parent directories if they don't exist
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write the file
        fs::write(&file_path, contents)?;

        if add_to_git {
            let mut index = self.repo_git2.index()?;
            index.add_path(file_path.strip_prefix(&self.path).unwrap())?;
            index.write()?;
        }

        Ok(TmpFile {
            repo: TmpRepo {
                path: self.path.clone(),
                repo_git2: Repository::open(&self.path)?,
                repo_gitai: crate::git::repository::find_repository_in_path(
                    self.path.to_str().unwrap(),
                )?,
            },
            filename: filename.to_string(),
            contents: contents.to_string(),
        })
    }

    /// Triggers a checkpoint with the given author
    pub fn trigger_checkpoint_with_author(
        &self,
        author: &str,
    ) -> Result<(usize, usize, usize), GitAiError> {
        checkpoint(
            &self.repo_gitai,
            author,
            CheckpointKind::Human,
            false, // show_working_log
            false, // reset
            true,
            None, // agent_run_result
            false,
        )
    }

    /// Triggers a checkpoint with AI content, creating proper prompts and agent data
    pub fn trigger_checkpoint_with_ai(
        &self,
        agent_name: &str,
        model: Option<&str>,
        tool: Option<&str>,
    ) -> Result<(usize, usize, usize), GitAiError> {
        use crate::authorship::transcript::AiTranscript;
        use crate::authorship::working_log::AgentId;
        use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;

        // Use a deterministic but unique session ID based on agent_name
        // For common agent names (Claude, GPT-4), use fixed ID for backwards compat
        // For unique names like "ai_session_1", use the name itself to allow distinct sessions
        let session_id =
            if agent_name == "Claude" || agent_name == "GPT-4" || agent_name == "GPT-4o" {
                "test_session_fixed".to_string()
            } else {
                agent_name.to_string()
            };

        // Create agent ID
        let agent_id = AgentId {
            tool: tool.unwrap_or("test_tool").to_string(),
            id: session_id.clone(),
            model: model.unwrap_or("test_model").to_string(),
        };

        // Create a minimal transcript with empty messages (as requested)
        let transcript = AiTranscript {
            messages: vec![], // Default to empty as requested
        };

        // Create agent run result
        let agent_run_result = AgentRunResult {
            agent_id,
            agent_metadata: None,
            transcript: Some(transcript),
            checkpoint_kind: CheckpointKind::AiAgent,
            repo_working_dir: None,
            edited_filepaths: None,
            will_edit_filepaths: None,
            dirty_files: None,
        };

        checkpoint(
            &self.repo_gitai,
            agent_name,
            CheckpointKind::AiAgent,
            false, // show_working_log
            false, // reset
            true,
            Some(agent_run_result),
            false,
        )
    }

    /// Triggers a checkpoint with a custom agent run result
    pub fn trigger_checkpoint_with_agent_result(
        &self,
        author: &str,
        agent_run_result: Option<AgentRunResult>,
    ) -> Result<(usize, usize, usize), GitAiError> {
        let checkpoint_kind = agent_run_result
            .as_ref()
            .map(|r| r.checkpoint_kind)
            .unwrap_or(CheckpointKind::Human);
        checkpoint(
            &self.repo_gitai,
            author,
            checkpoint_kind,
            false, // show_working_log
            false, // reset
            true,  // quiet
            agent_run_result,
            false,
        )
    }

    /// Commits all changes with the given message and runs post-commit hook
    pub fn commit_with_message(&self, message: &str) -> Result<AuthorshipLog, GitAiError> {
        // Add all files to the index
        let mut index = self.repo_git2.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;

        // Create the commit
        let tree_id = index.write_tree()?;
        let tree = self.repo_git2.find_tree(tree_id)?;

        // Use a fixed timestamp for stable test results
        // Unix timestamp for 2023-01-01 12:00:00 UTC
        let fixed_time = git2::Time::new(1672574400, 0);
        let signature = Signature::new("Test User", "test@example.com", &fixed_time)?;

        // Check if there's a parent commit before we use it
        let _has_parent = if let Ok(head) = self.repo_git2.head() {
            if let Some(target) = head.target() {
                self.repo_git2.find_commit(target).is_ok()
            } else {
                false
            }
        } else {
            false
        };

        // Get the current HEAD for the parent commit
        let parent_commit = if let Ok(head) = self.repo_git2.head() {
            if let Some(target) = head.target() {
                Some(self.repo_git2.find_commit(target)?)
            } else {
                None
            }
        } else {
            None
        };

        let (parent_sha, _commit_id) = if let Some(parent) = parent_commit {
            let parent_sha = Some(parent.id().to_string());
            let commit_id = self.repo_git2.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &[&parent],
            )?;
            (parent_sha, commit_id)
        } else {
            let commit_id =
                self.repo_git2
                    .commit(Some("HEAD"), &signature, &signature, message, &tree, &[])?;
            (None, commit_id)
        };

        println!("Commit ID: {}", _commit_id);

        // Run the post-commit hook for all commits (including initial commit)
        let post_commit_result = post_commit(
            &self.repo_gitai,
            parent_sha,
            _commit_id.to_string(),
            "Test User".to_string(),
            false,
        )?;

        Ok(post_commit_result.1)
    }

    /// Creates a new branch and switches to it
    pub fn create_branch(&self, branch_name: &str) -> Result<(), GitAiError> {
        let head = self.repo_git2.head()?;
        let commit = self.repo_git2.find_commit(head.target().unwrap())?;
        let _branch = self.repo_git2.branch(branch_name, &commit, false)?;

        // Switch to the new branch
        let branch_ref = self
            .repo_git2
            .find_reference(&format!("refs/heads/{}", branch_name))?;
        self.repo_git2.set_head(branch_ref.name().unwrap())?;

        // Update the working directory
        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts.force();
        self.repo_git2.checkout_head(Some(&mut checkout_opts))?;

        Ok(())
    }

    /// Switches to an existing branch
    pub fn switch_branch(&self, branch_name: &str) -> Result<(), GitAiError> {
        let branch_ref = self
            .repo_git2
            .find_reference(&format!("refs/heads/{}", branch_name))?;
        self.repo_git2.set_head(branch_ref.name().unwrap())?;

        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts.force();
        self.repo_git2.checkout_head(Some(&mut checkout_opts))?;

        Ok(())
    }

    /// Merges a branch into the current branch using real git CLI, always picking 'theirs' in conflicts
    pub fn merge_branch(&self, branch_name: &str, message: &str) -> Result<(), GitAiError> {
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(["merge", branch_name, "-m", message, "-X", "theirs"])
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git merge: {}", e)))?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git merge failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // Run post-commit hook
        // Get the merge commit SHA and its parent
        let head = self.repo_git2.head()?;
        let merge_commit_sha = head.target().unwrap().to_string();
        let merge_commit = self.repo_git2.find_commit(head.target().unwrap())?;
        let parent_sha = merge_commit.parent(0).ok().map(|p| p.id().to_string());

        post_commit(
            &self.repo_gitai,
            parent_sha,
            merge_commit_sha,
            "Test User".to_string(),
            false,
        )?;

        Ok(())
    }

    /// Rebases the current branch onto another branch using real git CLI, always picking 'theirs' in conflicts
    pub fn rebase_onto(&self, _base_branch: &str, onto_branch: &str) -> Result<(), GitAiError> {
        // First, get the current commit SHA before rebase
        // let old_sha = self.head_commit_sha()?;

        let mut rebase = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(["rebase", onto_branch])
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git rebase: {}", e)))?;

        // If rebase fails due to conflict, always pick 'theirs' and continue
        while !rebase.status.success()
            && String::from_utf8_lossy(&rebase.stderr).contains("could not apply")
        {
            // Find conflicted files (for our tests, just lines.md)
            let conflicted_file = self.path.join("lines.md");
            // Overwrite with theirs (the branch we're rebasing onto)
            let theirs_content = Command::new(crate::config::Config::get().git_cmd())
                .current_dir(&self.path)
                .args(["show", &format!("{}:lines.md", onto_branch)])
                .output()
                .map_err(|e| GitAiError::Generic(format!("Failed to get theirs: {}", e)))?;
            fs::write(&conflicted_file, &theirs_content.stdout)?;
            // Add and continue
            Command::new(crate::config::Config::get().git_cmd())
                .current_dir(&self.path)
                .args(["add", "lines.md"])
                .output()
                .map_err(|e| GitAiError::Generic(format!("Failed to git add: {}", e)))?;
            rebase = Command::new(crate::config::Config::get().git_cmd())
                .current_dir(&self.path)
                .args(["rebase", "--continue"])
                .output()
                .map_err(|e| {
                    GitAiError::Generic(format!("Failed to git rebase --continue: {}", e))
                })?;
        }

        if !rebase.status.success() {
            return Err(GitAiError::Generic(format!(
                "git rebase failed: {}",
                String::from_utf8_lossy(&rebase.stderr)
            )));
        }

        // Get the new commit SHA after rebase
        // let new_sha = self.head_commit_sha()?;

        // // Call the shared remapping function to update authorship logs
        // crate::log_fmt::authorship_log::remap_authorship_log_for_rewrite(
        //     &self.repo, &old_sha, &new_sha,
        // )?;

        // Run post-commit hook
        // Get the rebase commit SHA and its parent
        let head = self.repo_git2.head()?;
        let rebase_commit_sha = head.target().unwrap().to_string();
        let rebase_commit = self.repo_git2.find_commit(head.target().unwrap())?;
        let parent_sha = rebase_commit.parent(0).ok().map(|p| p.id().to_string());

        post_commit(
            &self.repo_gitai,
            parent_sha,
            rebase_commit_sha,
            "Test User".to_string(),
            false,
        )?;

        Ok(())
    }

    /// Gets the current branch name
    pub fn current_branch(&self) -> Result<String, GitAiError> {
        let head = self.repo_git2.head()?;
        let branch_name = head
            .shorthand()
            .ok_or_else(|| GitAiError::Generic("Could not get branch name".to_string()))?;
        Ok(branch_name.to_string())
    }

    /// Cherry-pick one or more commits
    pub fn cherry_pick(&self, commits: &[&str]) -> Result<(), GitAiError> {
        let mut args = vec!["cherry-pick"];
        args.extend(commits);

        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(&args)
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git cherry-pick: {}", e)))?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git cherry-pick failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    }

    /// Cherry-pick with expected conflicts (returns true if there were conflicts)
    pub fn cherry_pick_with_conflicts(&self, commit: &str) -> Result<bool, GitAiError> {
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(["cherry-pick", commit])
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git cherry-pick: {}", e)))?;

        // Check if there are conflicts (check both stderr and stdout)
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        let has_conflicts = !output.status.success()
            && (stderr.contains("conflict")
                || stdout.contains("conflict")
                || stderr.contains("CONFLICT")
                || stdout.contains("CONFLICT"));

        Ok(has_conflicts)
    }

    /// Continue a cherry-pick after resolving conflicts
    pub fn cherry_pick_continue(&self) -> Result<(), GitAiError> {
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(["cherry-pick", "--continue"])
            .env("GIT_EDITOR", "true") // Skip opening editor
            .output()
            .map_err(|e| {
                GitAiError::Generic(format!("Failed to run git cherry-pick --continue: {}", e))
            })?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git cherry-pick --continue failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    }

    /// Abort a cherry-pick operation
    pub fn cherry_pick_abort(&self) -> Result<(), GitAiError> {
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(["cherry-pick", "--abort"])
            .output()
            .map_err(|e| {
                GitAiError::Generic(format!("Failed to run git cherry-pick --abort: {}", e))
            })?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git cherry-pick --abort failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    }

    /// Gets the commit SHA of the current HEAD
    pub fn head_commit_sha(&self) -> Result<String, GitAiError> {
        let head = self.repo_git2.head()?;
        let commit_sha = head
            .target()
            .ok_or_else(|| GitAiError::Generic("No HEAD commit found".to_string()))?
            .to_string();
        Ok(commit_sha)
    }

    /// Stages a specific file
    pub fn stage_file(&self, filename: &str) -> Result<(), GitAiError> {
        let mut index = self.repo_git2.index()?;
        index.add_path(std::path::Path::new(filename))?;
        index.write()?;
        Ok(())
    }

    /// Unstages a specific file (resets it to HEAD)
    pub fn unstage_file(&self, filename: &str) -> Result<(), GitAiError> {
        let head = self.repo_git2.head()?;
        let commit = self.repo_git2.find_commit(head.target().unwrap())?;
        let tree = commit.tree()?;
        let tree_entry = tree.get_path(std::path::Path::new(filename))?;

        let mut index = self.repo_git2.index()?;
        index.add(&git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: tree_entry.filemode() as u32,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: tree_entry.id(),
            flags: 0,
            flags_extended: 0,
            path: filename.as_bytes().to_vec(),
        })?;
        index.write()?;
        Ok(())
    }

    /// Appends content to a file and stages it
    pub fn append_and_stage_file(
        &self,
        file: &mut TmpFile,
        content: &str,
    ) -> Result<(), GitAiError> {
        file.append(content)?;
        self.stage_file(&file.filename)?;
        Ok(())
    }

    /// Appends content to a file but keeps it unstaged
    ///
    /// This appends content to the working directory WITHOUT modifying the index.
    /// Whatever was previously staged remains staged, and the new content is unstaged.
    pub fn append_unstaged_file(
        &self,
        file: &mut TmpFile,
        content: &str,
    ) -> Result<(), GitAiError> {
        // Simply append to the working directory without touching the index
        // The index keeps whatever was previously staged (or points to HEAD if nothing was staged)
        file.append(content)?;
        Ok(())
    }

    /// Stages specific line ranges from a file (simulating `git add -p` behavior)
    ///
    /// This creates a staged version with only the specified line ranges from the working directory,
    /// while leaving other changes unstaged.
    ///
    /// # Arguments
    /// * `file` - The file to partially stage
    /// * `line_ranges` - Tuples of (start_line, end_line) to stage (1-indexed, inclusive)
    pub fn stage_lines_from_file(
        &self,
        file: &TmpFile,
        line_ranges: &[(usize, usize)],
    ) -> Result<(), GitAiError> {
        let file_path = self.path.join(&file.filename);

        // Read current working directory content
        let working_content = std::fs::read_to_string(&file_path)?;
        let working_lines: Vec<&str> = working_content.lines().collect();

        // Get the current HEAD version (or empty if new file)
        let head_content = {
            let head = self.repo_git2.head()?;
            let commit = self.repo_git2.find_commit(head.target().unwrap())?;
            let tree = commit.tree()?;

            match tree.get_path(std::path::Path::new(&file.filename)) {
                Ok(entry) => {
                    if let Ok(blob) = self.repo_git2.find_blob(entry.id()) {
                        String::from_utf8_lossy(blob.content()).to_string()
                    } else {
                        String::new()
                    }
                }
                Err(_) => String::new(),
            }
        };
        let head_lines: Vec<&str> = head_content.lines().collect();

        // Build the staged version by selecting lines from working directory or HEAD
        let mut staged_lines = Vec::new();

        // Determine which lines to take from working directory vs HEAD
        let max_lines = working_lines.len().max(head_lines.len());
        for line_num in 1..=max_lines {
            let should_stage = line_ranges
                .iter()
                .any(|(start, end)| line_num >= *start && line_num <= *end);

            if should_stage {
                // Take from working directory if available
                if line_num <= working_lines.len() {
                    staged_lines.push(working_lines[line_num - 1]);
                }
            } else {
                // Take from HEAD if available
                if line_num <= head_lines.len() {
                    staged_lines.push(head_lines[line_num - 1]);
                }
            }
        }

        // Create the staged content
        let mut staged_content = staged_lines.join("\n");
        if !staged_content.is_empty() {
            staged_content.push('\n');
        }

        // Create a blob with the staged content
        let blob_id = self.repo_git2.blob(staged_content.as_bytes())?;

        // Update the index with this blob
        let mut index = self.repo_git2.index()?;
        index.add(&git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644, // Regular file
            uid: 0,
            gid: 0,
            file_size: staged_content.len() as u32,
            id: blob_id,
            flags: 0,
            flags_extended: 0,
            path: file.filename.as_bytes().to_vec(),
        })?;
        index.write()?;

        Ok(())
    }

    /// Commits only staged changes with the given message and runs post-commit hook
    pub fn commit_staged_with_message(&self, message: &str) -> Result<AuthorshipLog, GitAiError> {
        // Get the current index (staged changes)
        let mut index = self.repo_git2.index()?;

        // Create the commit from staged changes only
        let tree_id = index.write_tree()?;
        let tree = self.repo_git2.find_tree(tree_id)?;

        // After write_tree, the index might get auto-updated. Clear and reload it from the tree
        // to ensure it matches exactly what we're committing
        index.clear()?;
        index.read_tree(&tree)?;
        index.write()?;

        // Use a fixed timestamp for stable test results
        let fixed_time = git2::Time::new(1672574400, 0);
        let signature = Signature::new("Test User", "test@example.com", &fixed_time)?;

        // Get the current HEAD for the parent commit
        let parent_commit = if let Ok(head) = self.repo_git2.head() {
            if let Some(target) = head.target() {
                Some(self.repo_git2.find_commit(target)?)
            } else {
                None
            }
        } else {
            None
        };

        let (parent_sha, _commit_id) = if let Some(parent) = parent_commit {
            let parent_sha = Some(parent.id().to_string());
            let commit_id = self.repo_git2.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &[&parent],
            )?;
            (parent_sha, commit_id)
        } else {
            let commit_id =
                self.repo_git2
                    .commit(Some("HEAD"), &signature, &signature, message, &tree, &[])?;
            (None, commit_id)
        };

        // Run the post-commit hook
        let post_commit_result = post_commit(
            &self.repo_gitai,
            parent_sha,
            _commit_id.to_string(),
            "Test User".to_string(),
            false,
        )?;

        Ok(post_commit_result.1)
    }

    /// Gets the default branch name (first branch created)
    pub fn get_default_branch(&self) -> Result<String, GitAiError> {
        // Try to find the first branch that's not the current one
        let current = self.current_branch()?;

        // List all references and find the first branch
        let refs = self.repo_git2.references()?;
        for reference in refs {
            let reference = reference?;
            if let Some(name) = reference.name()
                && name.starts_with("refs/heads/")
            {
                let branch_name = name.strip_prefix("refs/heads/").unwrap();
                if branch_name != current {
                    return Ok(branch_name.to_string());
                }
            }
        }

        // If no other branch found, return current
        Ok(current)
    }

    /// Gets the repository path
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Gets a reference to the underlying git2 Repository
    pub fn repo(&self) -> &Repository {
        &self.repo_git2
    }

    /// Runs blame on a file in the repository
    pub fn blame_for_file(
        &self,
        tmp_file: &TmpFile,
        line_range: Option<(u32, u32)>,
    ) -> Result<BTreeMap<u32, String>, GitAiError> {
        // Use the filename (relative path) instead of the absolute path
        // Convert the blame result to BTreeMap for deterministic order
        let mut options = blame::GitAiBlameOptions::default();
        if let Some((start, end)) = line_range {
            options.line_ranges.push((start, end));
        }

        // Set pager environment variables to avoid interactive pager in tests
        unsafe {
            std::env::set_var("GIT_PAGER", "cat");
            std::env::set_var("PAGER", "cat");
        }

        let (blame_map, _) = self.repo_gitai.blame(&tmp_file.filename, &options)?;
        println!("blame_map: {:?}", blame_map);
        Ok(blame_map.into_iter().collect())
    }

    /// Gets the authorship log for the current commit
    pub fn get_authorship_log(
        &self,
    ) -> Result<crate::authorship::authorship_log_serialization::AuthorshipLog, GitAiError> {
        let head = self.repo_git2.head()?;
        let commit_id = head.target().unwrap().to_string();
        match crate::git::refs::show_authorship_note(&self.repo_gitai, &commit_id) {
            Some(content) => {
                // Parse the authorship log from the note content
                crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(&content)
                    .map_err(|e| GitAiError::Generic(format!("Failed to parse authorship log: {}", e)))
            }
            None => Err(GitAiError::Generic("No authorship log found".to_string())),
        }
    }

    /// Gets the HEAD commit SHA (alias for head_commit_sha for convenience)
    pub fn get_head_commit_sha(&self) -> Result<String, GitAiError> {
        self.head_commit_sha()
    }

    /// Gets a reference to the gitai Repository
    pub fn gitai_repo(&self) -> &crate::git::repository::Repository {
        &self.repo_gitai
    }

    /// Adds a remote with the given name and URL
    pub fn add_remote(&self, name: &str, url: &str) -> Result<(), GitAiError> {
        self.repo_git2
            .remote(name, url)
            .map_err(|e| GitAiError::Generic(format!("Failed to add remote: {}", e)))?;
        Ok(())
    }

    /// Amends the current commit with the staged changes and returns the new commit SHA
    pub fn amend_commit(&self, message: &str) -> Result<String, GitAiError> {
        // Get the current HEAD commit that we're amending
        let head = self.repo_git2.head()?;
        let _current_commit = self.repo_git2.find_commit(head.target().unwrap())?;

        // Use git CLI to amend the commit (this is simpler and more reliable)
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args([
                "commit",
                "--amend",
                "-m",
                message,
                "--allow-empty",
                "--no-verify",
            ])
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git commit --amend: {}", e)))?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git commit --amend failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // Get the new commit SHA after amending
        let new_head = self.repo_git2.head()?;
        let new_commit_sha = new_head.target().unwrap().to_string();

        Ok(new_commit_sha)
    }

    /// Alias for switch_branch - checks out an existing branch
    pub fn checkout_branch(&self, branch_name: &str) -> Result<(), GitAiError> {
        self.switch_branch(branch_name)
    }

    /// Performs a squash merge of a branch into the current branch (stages changes without committing)
    pub fn merge_squash(&self, branch_name: &str) -> Result<(), GitAiError> {
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(["merge", "--squash", branch_name])
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git merge --squash: {}", e)))?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git merge --squash failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    }

    /// Merges a branch into the current branch, allowing conflicts to remain unresolved
    /// Returns Ok(true) if there are conflicts, Ok(false) if merge succeeded without conflicts
    pub fn merge_with_conflicts(&self, branch_name: &str) -> Result<bool, GitAiError> {
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(["merge", branch_name, "--no-commit"])
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git merge: {}", e)))?;

        // Exit code 1 with "conflict" in output means there are merge conflicts
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success()
            && (stderr.contains("conflict")
                || stdout.contains("conflict")
                || stderr.contains("CONFLICT")
                || stdout.contains("CONFLICT"))
        {
            // Conflicts exist - this is expected
            return Ok(true);
        }

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git merge failed unexpectedly: {}",
                stderr
            )));
        }

        // Merge succeeded without conflicts
        Ok(false)
    }

    /// Resolves a conflicted file by choosing one version (ours or theirs)
    pub fn resolve_conflict(&self, filename: &str, choose: &str) -> Result<(), GitAiError> {
        match choose {
            "ours" => {
                let output = Command::new(crate::config::Config::get().git_cmd())
                    .current_dir(&self.path)
                    .args(["checkout", "--ours", filename])
                    .output()
                    .map_err(|e| {
                        GitAiError::Generic(format!("Failed to checkout --ours: {}", e))
                    })?;

                if !output.status.success() {
                    return Err(GitAiError::Generic(format!(
                        "git checkout --ours failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }
            }
            "theirs" => {
                let output = Command::new(crate::config::Config::get().git_cmd())
                    .current_dir(&self.path)
                    .args(["checkout", "--theirs", filename])
                    .output()
                    .map_err(|e| {
                        GitAiError::Generic(format!("Failed to checkout --theirs: {}", e))
                    })?;

                if !output.status.success() {
                    return Err(GitAiError::Generic(format!(
                        "git checkout --theirs failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }
            }
            _ => {
                return Err(GitAiError::Generic(format!(
                    "Invalid choice: {}. Use 'ours' or 'theirs'",
                    choose
                )));
            }
        }

        // Stage the resolved file
        self.stage_file(filename)?;
        Ok(())
    }

    /// Execute a git command directly (no hooks)
    pub fn git_command(&self, args: &[&str]) -> Result<(), GitAiError> {
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(args)
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git command: {}", e)))?;

        if !output.status.success() {
            return Err(GitAiError::Generic(format!(
                "git command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    }

    /// Execute git reset with git-ai hooks
    pub fn reset(
        &self,
        target: &str,
        mode: ResetMode,
        pathspecs: &[&str],
    ) -> Result<(), GitAiError> {
        // Capture HEAD before reset
        let mut repo_mut =
            crate::git::repository::find_repository_in_path(self.path.to_str().unwrap())?;
        repo_mut.require_pre_command_head();

        // Build git command args
        let mut args = vec!["reset".to_string()];

        match mode {
            ResetMode::Hard => args.push("--hard".to_string()),
            ResetMode::Soft => args.push("--soft".to_string()),
            ResetMode::Mixed => args.push("--mixed".to_string()),
            ResetMode::Merge => args.push("--merge".to_string()),
            ResetMode::Keep => args.push("--keep".to_string()),
        }

        args.push(target.to_string());

        for pathspec in pathspecs {
            args.push(pathspec.to_string());
        }

        // Run the actual git command
        let output = Command::new(crate::config::Config::get().git_cmd())
            .current_dir(&self.path)
            .args(&args)
            .output()
            .map_err(|e| GitAiError::Generic(format!("Failed to run git reset: {}", e)))?;

        let exit_status = output.status;

        if !exit_status.success() {
            return Err(GitAiError::Generic(format!(
                "git reset failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // Call post-reset hook directly
        let parsed_args = crate::git::cli_parser::parse_git_cli_args(&args);
        crate::commands::hooks::reset_hooks::post_reset_hook(
            &parsed_args,
            &mut repo_mut,
            exit_status,
        );

        Ok(())
    }
}

// @todo move this acunniffe
/// Sanitized checkpoint representation for deterministic snapshots
#[allow(dead_code)]
#[derive(Debug)]
pub struct SnapshotCheckpoint {
    author: String,
    has_agent: bool,
    agent_tool: Option<String>,
    entries: Vec<SnapshotEntry>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct SnapshotEntry {
    file: String,
    attributions: Vec<Attribution>,
}

#[allow(dead_code)]
pub fn snapshot_checkpoints(checkpoints: &[Checkpoint]) -> Vec<SnapshotCheckpoint> {
    let mut snapshots: Vec<SnapshotCheckpoint> = checkpoints
        .iter()
        .map(|cp| {
            let mut entries: Vec<SnapshotEntry> = cp
                .entries
                .iter()
                .map(|e| {
                    let mut attributions = e.attributions.clone();
                    // Sort attributions by start position, then end position, then author_id for determinism
                    attributions.sort_by(|a, b| {
                        a.start
                            .cmp(&b.start)
                            .then_with(|| a.end.cmp(&b.end))
                            .then_with(|| a.author_id.cmp(&b.author_id))
                    });

                    SnapshotEntry {
                        file: e.file.clone(),
                        attributions,
                    }
                })
                .collect();

            // Sort entries by file name for deterministic ordering
            entries.sort_by(|a, b| a.file.cmp(&b.file));

            SnapshotCheckpoint {
                author: cp.author.clone(),
                has_agent: cp.agent_id.is_some(),
                agent_tool: cp.agent_id.as_ref().map(|a| a.tool.clone()),
                entries,
            }
        })
        .collect();

    // Sort checkpoints by author name, then by first file name, then by first attribution start position
    // for deterministic ordering
    snapshots.sort_by(|a, b| {
        // First sort by author
        match a.author.cmp(&b.author) {
            std::cmp::Ordering::Equal => {
                // If authors are equal, sort by first file name
                let a_file = a.entries.first().map(|e| e.file.as_str()).unwrap_or("");
                let b_file = b.entries.first().map(|e| e.file.as_str()).unwrap_or("");
                match a_file.cmp(b_file) {
                    std::cmp::Ordering::Equal => {
                        // If files are equal, sort by first attribution start position
                        let a_start = a
                            .entries
                            .first()
                            .and_then(|e| e.attributions.first())
                            .map(|attr| attr.start)
                            .unwrap_or(0);
                        let b_start = b
                            .entries
                            .first()
                            .and_then(|e| e.attributions.first())
                            .map(|attr| attr.start)
                            .unwrap_or(0);
                        a_start.cmp(&b_start)
                    }
                    other => other,
                }
            }
            other => other,
        }
    });

    snapshots
}

/// Reset mode for git reset command
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum ResetMode {
    Hard,
    Soft,
    Mixed,
    Merge,
    Keep,
}

#[allow(dead_code)]
const ALPHABET: &str = "A
B
C
D
E
F
G
H
I
J
K
L
M
N
O
P
Q
R
S
T
U
V
W
X
Y
Z";

#[allow(dead_code)]
const LINES: &str = "1
2
3
4
5
6
7
8
9
10
11
12
13
14
15
16
17
18
19
20
21
22
23
24
25
26
27
28
29
30
31
32
33";
