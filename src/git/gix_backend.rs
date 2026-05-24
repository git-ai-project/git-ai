use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gix::ObjectId;
use gix::bstr::BStr;

use crate::error::GitAiError;

/// Backend that uses `gix` for native git object reads (both loose and packed).
///
/// This replaces the loose-object-only FastReader approach with full packfile
/// support, eliminating the need to spawn `git` subprocesses for packed objects.
pub struct GixBackend {
    git_dir: PathBuf,
    repo: OnceLock<Result<gix::ThreadSafeRepository, String>>,
}

impl std::fmt::Debug for GixBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GixBackend")
            .field("git_dir", &self.git_dir)
            .finish_non_exhaustive()
    }
}

impl Clone for GixBackend {
    fn clone(&self) -> Self {
        Self {
            git_dir: self.git_dir.clone(),
            repo: OnceLock::new(),
        }
    }
}

impl GixBackend {
    pub fn new(git_dir: &Path) -> Self {
        Self {
            git_dir: git_dir.to_path_buf(),
            repo: OnceLock::new(),
        }
    }

    fn get_repo(&self) -> Result<gix::Repository, GitAiError> {
        let result = self.repo.get_or_init(|| {
            let opts = gix::open::Options::isolated();
            gix::open_opts(&self.git_dir, opts)
                .map(|r| r.into())
                .map_err(|e| e.to_string())
        });
        match result {
            Ok(ts_repo) => Ok(ts_repo.to_thread_local()),
            Err(e) => Err(GitAiError::GixError(e.clone())),
        }
    }

    fn parse_oid(oid_hex: &str) -> Result<ObjectId, GitAiError> {
        ObjectId::from_hex(oid_hex.as_bytes())
            .map_err(|e| GitAiError::GixError(format!("invalid OID '{}': {}", oid_hex, e)))
    }

    // --- Public API: Result-returning methods ---

    pub fn read_blob(&self, oid_hex: &str) -> Result<Vec<u8>, GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(oid_hex)?;
        let object = repo
            .find_object(oid)
            .map_err(|e| GitAiError::GixError(format!("find object '{}': {}", oid_hex, e)))?;
        if object.kind != gix::object::Kind::Blob {
            return Err(GitAiError::GixError(format!(
                "object '{}' is not a blob (found {:?})",
                oid_hex, object.kind
            )));
        }
        Ok(object.detach().data)
    }

    pub fn read_commit_tree_oid(&self, commit_oid_hex: &str) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(commit_oid_hex)?;
        let object = repo.find_object(oid).map_err(|e| {
            GitAiError::GixError(format!("find commit '{}': {}", commit_oid_hex, e))
        })?;
        let commit = object.try_into_commit().map_err(|e| {
            GitAiError::GixError(format!(
                "object '{}' is not a commit: {}",
                commit_oid_hex, e
            ))
        })?;
        Ok(commit
            .tree_id()
            .map_err(|e| {
                GitAiError::GixError(format!("read tree from commit '{}': {}", commit_oid_hex, e))
            })?
            .to_string())
    }

    pub fn object_kind(&self, oid_hex: &str) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(oid_hex)?;
        let object = repo
            .find_object(oid)
            .map_err(|e| GitAiError::GixError(format!("find object '{}': {}", oid_hex, e)))?;
        let kind_str = match object.kind {
            gix::object::Kind::Blob => "blob",
            gix::object::Kind::Tree => "tree",
            gix::object::Kind::Commit => "commit",
            gix::object::Kind::Tag => "tag",
        };
        Ok(kind_str.to_string())
    }

    pub fn tree_entry_for_path(
        &self,
        tree_oid_hex: &str,
        path: &str,
    ) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(tree_oid_hex)?;
        let object = repo
            .find_object(oid)
            .map_err(|e| GitAiError::GixError(format!("find tree '{}': {}", tree_oid_hex, e)))?;
        let tree = object.try_into_tree().map_err(|e| {
            GitAiError::GixError(format!("object '{}' is not a tree: {}", tree_oid_hex, e))
        })?;
        let entry = tree.lookup_entry_by_path(path).map_err(|e| {
            GitAiError::GixError(format!(
                "lookup path '{}' in tree '{}': {}",
                path, tree_oid_hex, e
            ))
        })?;
        match entry {
            Some(e) => Ok(e.object_id().to_string()),
            None => Err(GitAiError::GixError(format!(
                "path '{}' not found in tree '{}'",
                path, tree_oid_hex
            ))),
        }
    }

    pub fn resolve_ref(&self, refname: &str) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let reference = repo
            .find_reference(refname)
            .map_err(|e| GitAiError::GixError(format!("find reference '{}': {}", refname, e)))?;
        let oid = reference
            .into_fully_peeled_id()
            .map_err(|e| GitAiError::GixError(format!("peel reference '{}': {}", refname, e)))?;
        Ok(oid.to_string())
    }

    pub fn head_ref_name(&self) -> Result<Option<String>, GitAiError> {
        let repo = self.get_repo()?;
        let head = repo
            .head_ref()
            .map_err(|e| GitAiError::GixError(format!("read HEAD ref: {}", e)))?;
        Ok(head.map(|r| r.name().as_bstr().to_string()))
    }

    pub fn head_commit_oid(&self) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let head = repo
            .head_id()
            .map_err(|e| GitAiError::GixError(format!("read HEAD commit: {}", e)))?;
        Ok(head.to_string())
    }

    pub fn commit_parent_ids(&self, commit_oid_hex: &str) -> Result<Vec<String>, GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(commit_oid_hex)?;
        let object = repo.find_object(oid).map_err(|e| {
            GitAiError::GixError(format!("find commit '{}': {}", commit_oid_hex, e))
        })?;
        let commit = object.try_into_commit().map_err(|e| {
            GitAiError::GixError(format!(
                "object '{}' is not a commit: {}",
                commit_oid_hex, e
            ))
        })?;
        let parent_ids: Vec<String> = commit.parent_ids().map(|id| id.to_string()).collect();
        Ok(parent_ids)
    }

    pub fn commit_message(
        &self,
        commit_oid_hex: &str,
    ) -> Result<(String, Option<String>), GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(commit_oid_hex)?;
        let object = repo.find_object(oid).map_err(|e| {
            GitAiError::GixError(format!("find commit '{}': {}", commit_oid_hex, e))
        })?;
        let commit = object.try_into_commit().map_err(|e| {
            GitAiError::GixError(format!(
                "object '{}' is not a commit: {}",
                commit_oid_hex, e
            ))
        })?;
        let message = commit.message().map_err(|e| {
            GitAiError::GixError(format!(
                "read message from commit '{}': {}",
                commit_oid_hex, e
            ))
        })?;
        let summary = message.summary().to_string();
        let body = message.body.map(|b| b.to_string());
        Ok((summary, body))
    }

    /// Get the author name and email from a commit.
    pub fn commit_author(&self, commit_oid_hex: &str) -> Result<(String, String), GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(commit_oid_hex)?;
        let object = repo.find_object(oid).map_err(|e| {
            GitAiError::GixError(format!("find commit '{}': {}", commit_oid_hex, e))
        })?;
        let commit = object.try_into_commit().map_err(|e| {
            GitAiError::GixError(format!(
                "object '{}' is not a commit: {}",
                commit_oid_hex, e
            ))
        })?;
        let author = commit.author().map_err(|e| {
            GitAiError::GixError(format!(
                "read author from commit '{}': {}",
                commit_oid_hex, e
            ))
        })?;
        let name = author.name.to_string();
        let email = author.email.to_string();
        Ok((name, email))
    }

    pub fn rev_parse(&self, spec: &str) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let bstr_spec: &BStr = spec.into();
        let id = repo
            .rev_parse_single(bstr_spec)
            .map_err(|e| GitAiError::GixError(format!("rev-parse '{}': {}", spec, e)))?;
        Ok(id.to_string())
    }

    pub fn read_blobs_by_oids(
        &self,
        pairs: &[(&str, &str)],
    ) -> Result<Vec<(String, Vec<u8>)>, GitAiError> {
        let repo = self.get_repo()?;
        let mut results = Vec::with_capacity(pairs.len());
        for &(path, oid_hex) in pairs {
            let oid = Self::parse_oid(oid_hex)?;
            let object = repo.find_object(oid).map_err(|e| {
                GitAiError::GixError(format!(
                    "find blob '{}' for path '{}': {}",
                    oid_hex, path, e
                ))
            })?;
            results.push((path.to_string(), object.detach().data));
        }
        Ok(results)
    }

    /// Read a file's content at a given commit (equivalent to `git show <commit>:<path>`).
    pub fn read_file_at_commit(
        &self,
        commit_spec: &str,
        path: &str,
    ) -> Result<Vec<u8>, GitAiError> {
        let repo = self.get_repo()?;

        // Resolve the commit spec to a commit OID
        let bstr_spec: &BStr = commit_spec.into();
        let commit_id = repo
            .rev_parse_single(bstr_spec)
            .map_err(|e| GitAiError::GixError(format!("rev-parse '{}': {}", commit_spec, e)))?;

        // Get the commit's tree
        let object = repo
            .find_object(commit_id)
            .map_err(|e| GitAiError::GixError(format!("find object '{}': {}", commit_spec, e)))?;
        let commit = object
            .try_into_commit()
            .map_err(|e| GitAiError::GixError(format!("not a commit '{}': {}", commit_spec, e)))?;
        let tree_id = commit.tree_id().map_err(|e| {
            GitAiError::GixError(format!("read tree from '{}': {}", commit_spec, e))
        })?;
        let tree = repo
            .find_object(tree_id)
            .map_err(|e| GitAiError::GixError(format!("find tree: {}", e)))?
            .try_into_tree()
            .map_err(|e| GitAiError::GixError(format!("not a tree: {}", e)))?;

        // Look up path in tree
        let entry = tree
            .lookup_entry_by_path(path)
            .map_err(|e| GitAiError::GixError(format!("lookup '{}' in tree: {}", path, e)))?;
        let entry = entry.ok_or_else(|| {
            GitAiError::GixError(format!("path '{}' not found at '{}'", path, commit_spec))
        })?;

        // Read the blob
        let blob_obj = repo
            .find_object(entry.object_id())
            .map_err(|e| GitAiError::GixError(format!("read blob for '{}': {}", path, e)))?;
        Ok(blob_obj.detach().data)
    }

    /// Compute the merge base of two commits.
    pub fn merge_base(&self, one: &str, two: &str) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let bstr_one: &BStr = one.into();
        let bstr_two: &BStr = two.into();
        let id_one = repo
            .rev_parse_single(bstr_one)
            .map_err(|e| GitAiError::GixError(format!("rev-parse '{}': {}", one, e)))?;
        let id_two = repo
            .rev_parse_single(bstr_two)
            .map_err(|e| GitAiError::GixError(format!("rev-parse '{}': {}", two, e)))?;
        let base = repo
            .merge_base(id_one, id_two)
            .map_err(|e| GitAiError::GixError(format!("merge-base '{}' '{}': {}", one, two, e)))?;
        Ok(base.to_string())
    }

    /// Peel an object to a commit OID (equivalent to `git rev-parse <oid>^{commit}`).
    pub fn peel_to_commit(&self, oid_hex: &str) -> Result<String, GitAiError> {
        let repo = self.get_repo()?;
        let oid = Self::parse_oid(oid_hex)?;
        let object = repo
            .find_object(oid)
            .map_err(|e| GitAiError::GixError(format!("find object '{}': {}", oid_hex, e)))?;
        match object.kind {
            gix::object::Kind::Commit => Ok(oid_hex.to_string()),
            gix::object::Kind::Tag => {
                let tag = object
                    .try_into_tag()
                    .map_err(|e| GitAiError::GixError(format!("peel tag '{}': {}", oid_hex, e)))?;
                let target = tag.target_id().map_err(|e| {
                    GitAiError::GixError(format!("tag target '{}': {}", oid_hex, e))
                })?;
                // Recursively peel (tags can point to tags)
                self.peel_to_commit(&target.to_string())
            }
            _ => Err(GitAiError::GixError(format!(
                "object '{}' ({:?}) cannot be peeled to commit",
                oid_hex, object.kind
            ))),
        }
    }

    /// Read a git note for a given object under a notes ref.
    /// Notes are stored in a tree under the notes ref, with the path being
    /// either flat (sha) or fanout (xx/yyyyyyyy...).
    pub fn read_note(&self, notes_ref: &str, object_sha: &str) -> Result<Vec<u8>, GitAiError> {
        let repo = self.get_repo()?;

        // Resolve the notes ref to a commit, then get its tree
        let reference = repo
            .find_reference(notes_ref)
            .map_err(|e| GitAiError::GixError(format!("find notes ref '{}': {}", notes_ref, e)))?;
        let commit_id = reference
            .into_fully_peeled_id()
            .map_err(|e| GitAiError::GixError(format!("peel notes ref '{}': {}", notes_ref, e)))?;
        let commit_obj = repo
            .find_object(commit_id)
            .map_err(|e| GitAiError::GixError(format!("find notes commit: {}", e)))?;
        let commit = commit_obj
            .try_into_commit()
            .map_err(|e| GitAiError::GixError(format!("notes ref not a commit: {}", e)))?;
        let tree_id = commit
            .tree_id()
            .map_err(|e| GitAiError::GixError(format!("notes commit tree: {}", e)))?;
        let tree = repo
            .find_object(tree_id)
            .map_err(|e| GitAiError::GixError(format!("find notes tree: {}", e)))?
            .try_into_tree()
            .map_err(|e| GitAiError::GixError(format!("not a tree: {}", e)))?;

        // Try fanout path first (xx/yyyyyyyy...), then flat path
        let fanout_path = if object_sha.len() > 2 {
            format!("{}/{}", &object_sha[..2], &object_sha[2..])
        } else {
            object_sha.to_string()
        };

        let entry = tree
            .lookup_entry_by_path(&fanout_path)
            .map_err(|e| GitAiError::GixError(format!("lookup note: {}", e)))?
            .or_else(|| tree.lookup_entry_by_path(object_sha).ok().flatten());

        let entry =
            entry.ok_or_else(|| GitAiError::GixError(format!("no note for '{}'", object_sha)))?;

        let blob = repo
            .find_object(entry.object_id())
            .map_err(|e| GitAiError::GixError(format!("read note blob: {}", e)))?;
        Ok(blob.detach().data)
    }

    /// Batch-lookup note blob OIDs for multiple objects under a notes ref.
    /// Returns a map of object_sha → note_blob_oid for objects that have notes.
    pub fn note_blob_oids_batch(
        &self,
        notes_ref: &str,
        object_shas: &[String],
    ) -> Result<std::collections::HashMap<String, String>, GitAiError> {
        let repo = self.get_repo()?;
        let reference = repo
            .find_reference(notes_ref)
            .map_err(|e| GitAiError::GixError(format!("find notes ref '{}': {}", notes_ref, e)))?;
        let commit_id = reference
            .into_fully_peeled_id()
            .map_err(|e| GitAiError::GixError(format!("peel notes ref '{}': {}", notes_ref, e)))?;
        let commit_obj = repo
            .find_object(commit_id)
            .map_err(|e| GitAiError::GixError(format!("find notes commit: {}", e)))?;
        let commit = commit_obj
            .try_into_commit()
            .map_err(|e| GitAiError::GixError(format!("notes ref not a commit: {}", e)))?;
        let tree_id = commit
            .tree_id()
            .map_err(|e| GitAiError::GixError(format!("notes commit tree: {}", e)))?;
        let tree = repo
            .find_object(tree_id)
            .map_err(|e| GitAiError::GixError(format!("find notes tree: {}", e)))?
            .try_into_tree()
            .map_err(|e| GitAiError::GixError(format!("not a tree: {}", e)))?;

        let mut result = std::collections::HashMap::new();
        for sha in object_shas {
            let fanout_path = if sha.len() > 2 {
                format!("{}/{}", &sha[..2], &sha[2..])
            } else {
                sha.to_string()
            };
            let entry = tree
                .lookup_entry_by_path(&fanout_path)
                .ok()
                .flatten()
                .or_else(|| tree.lookup_entry_by_path(sha.as_str()).ok().flatten());
            if let Some(e) = entry {
                result.insert(sha.clone(), e.object_id().to_string());
            }
        }
        Ok(result)
    }

    /// Check if a ref exists in the repository.
    pub fn ref_exists(&self, refname: &str) -> Result<bool, GitAiError> {
        let repo = self.get_repo()?;
        match repo.find_reference(refname) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Get the current branch short name (equivalent to `git branch --show-current`).
    pub fn head_branch_short_name(&self) -> Result<Option<String>, GitAiError> {
        let repo = self.get_repo()?;
        let head = repo
            .head_ref()
            .map_err(|e| GitAiError::GixError(format!("read HEAD ref: {}", e)))?;
        Ok(head.map(|r| {
            let full = r.name().as_bstr().to_string();
            full.strip_prefix("refs/heads/")
                .unwrap_or(&full)
                .to_string()
        }))
    }

    // --- Public API: Option-returning convenience methods ---

    pub fn try_read_blob(&self, oid_hex: &str) -> Option<Vec<u8>> {
        self.read_blob(oid_hex).ok()
    }

    pub fn try_read_commit_tree_oid(&self, commit_oid_hex: &str) -> Option<String> {
        self.read_commit_tree_oid(commit_oid_hex).ok()
    }

    pub fn try_object_kind(&self, oid_hex: &str) -> Option<String> {
        self.object_kind(oid_hex).ok()
    }

    pub fn try_tree_entry_for_path(&self, tree_oid_hex: &str, path: &str) -> Option<String> {
        self.tree_entry_for_path(tree_oid_hex, path).ok()
    }

    pub fn try_resolve_ref(&self, refname: &str) -> Option<String> {
        self.resolve_ref(refname).ok()
    }

    pub fn try_rev_parse(&self, spec: &str) -> Option<String> {
        self.rev_parse(spec).ok()
    }

    pub fn try_read_file_at_commit(&self, commit_spec: &str, path: &str) -> Option<Vec<u8>> {
        self.read_file_at_commit(commit_spec, path).ok()
    }

    pub fn try_merge_base(&self, one: &str, two: &str) -> Option<String> {
        self.merge_base(one, two).ok()
    }

    pub fn try_peel_to_commit(&self, oid_hex: &str) -> Option<String> {
        self.peel_to_commit(oid_hex).ok()
    }

    /// Walk commits from `tip` back to (but not including) `base`.
    /// Returns commits in newest-first order (like `git rev-list base..tip`).
    pub fn rev_list(&self, tip: &str, base: &str) -> Result<Vec<String>, GitAiError> {
        let repo = self.get_repo()?;
        let bstr_tip: &BStr = tip.into();
        let bstr_base: &BStr = base.into();
        let tip_id = repo
            .rev_parse_single(bstr_tip)
            .map_err(|e| GitAiError::GixError(format!("rev-parse tip '{}': {}", tip, e)))?;
        let base_id = repo
            .rev_parse_single(bstr_base)
            .map_err(|e| GitAiError::GixError(format!("rev-parse base '{}': {}", base, e)))?;

        let walk = repo
            .rev_walk([tip_id])
            .sorting(gix::revision::walk::Sorting::BreadthFirst)
            .selected(move |oid| oid != base_id.as_ref())
            .map_err(|e| GitAiError::GixError(format!("rev_walk '{}..{}': {}", base, tip, e)))?;

        let mut commits = Vec::new();
        for info in walk {
            let info = info.map_err(|e| GitAiError::GixError(format!("rev_walk iter: {}", e)))?;
            commits.push(info.id.to_string());
        }
        Ok(commits)
    }

    /// Walk commits from `tip` back to (but not including) `base`, first-parent only.
    /// Returns commits in newest-first order with a maximum count.
    pub fn rev_list_first_parent(
        &self,
        tip: &str,
        base: &str,
        max_count: usize,
    ) -> Result<Vec<String>, GitAiError> {
        let repo = self.get_repo()?;
        let bstr_tip: &BStr = tip.into();
        let bstr_base: &BStr = base.into();
        let tip_id = repo
            .rev_parse_single(bstr_tip)
            .map_err(|e| GitAiError::GixError(format!("rev-parse tip '{}': {}", tip, e)))?;
        let base_id = repo
            .rev_parse_single(bstr_base)
            .map_err(|e| GitAiError::GixError(format!("rev-parse base '{}': {}", base, e)))?;

        let walk = repo
            .rev_walk([tip_id])
            .sorting(gix::revision::walk::Sorting::BreadthFirst)
            .first_parent_only()
            .selected(move |oid| oid != base_id.as_ref())
            .map_err(|e| {
                GitAiError::GixError(format!("rev_walk first-parent '{}..{}': {}", base, tip, e))
            })?;

        let mut commits = Vec::new();
        for info in walk {
            let info = info.map_err(|e| GitAiError::GixError(format!("rev_walk iter: {}", e)))?;
            commits.push(info.id.to_string());
            if commits.len() >= max_count {
                break;
            }
        }
        Ok(commits)
    }

    /// Check if `ancestor` is an ancestor of `descendant`.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, GitAiError> {
        let base = self.merge_base(ancestor, descendant)?;
        let canonical = self.rev_parse(ancestor)?;
        Ok(base == canonical)
    }

    /// Get list of changed file paths between two trees (equivalent to
    /// `git diff-tree --name-only -r tree1 tree2`).
    pub fn diff_tree_changed_files(
        &self,
        from_tree_oid: &str,
        to_tree_oid: &str,
    ) -> Result<Vec<String>, GitAiError> {
        use gix::objs::TreeRefIter;

        let repo = self.get_repo()?;
        let from_oid = Self::parse_oid(from_tree_oid)?;
        let to_oid = Self::parse_oid(to_tree_oid)?;

        let from_obj = repo
            .find_object(from_oid)
            .map_err(|e| GitAiError::GixError(format!("find tree '{}': {}", from_tree_oid, e)))?;
        let to_obj = repo
            .find_object(to_oid)
            .map_err(|e| GitAiError::GixError(format!("find tree '{}': {}", to_tree_oid, e)))?;

        let from_data = from_obj.detach();
        let to_data = to_obj.detach();
        let from_iter = TreeRefIter::from_bytes(&from_data.data);
        let to_iter = TreeRefIter::from_bytes(&to_data.data);

        let mut recorder = gix::diff::tree::Recorder::default();
        let mut state = gix::diff::tree::State::default();
        (gix::diff::tree)(from_iter, to_iter, &mut state, &repo.objects, &mut recorder)
            .map_err(|e| GitAiError::GixError(format!("diff-tree: {}", e)))?;

        let mut files: Vec<String> = recorder
            .records
            .into_iter()
            .map(|change| match change {
                gix::diff::tree::recorder::Change::Addition { path, .. }
                | gix::diff::tree::recorder::Change::Deletion { path, .. }
                | gix::diff::tree::recorder::Change::Modification { path, .. } => path.to_string(),
            })
            .collect();
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Get changed files between two commits by comparing their trees.
    pub fn diff_commits_changed_files(
        &self,
        from_commit: &str,
        to_commit: &str,
    ) -> Result<Vec<String>, GitAiError> {
        let from_tree = self.read_commit_tree_oid(from_commit)?;
        let to_tree = self.read_commit_tree_oid(to_commit)?;
        self.diff_tree_changed_files(&from_tree, &to_tree)
    }

    pub fn try_rev_list(&self, tip: &str, base: &str) -> Option<Vec<String>> {
        self.rev_list(tip, base).ok()
    }

    pub fn try_is_ancestor(&self, ancestor: &str, descendant: &str) -> Option<bool> {
        self.is_ancestor(ancestor, descendant).ok()
    }

    pub fn try_diff_commits_changed_files(
        &self,
        from_commit: &str,
        to_commit: &str,
    ) -> Option<Vec<String>> {
        self.diff_commits_changed_files(from_commit, to_commit).ok()
    }
}
