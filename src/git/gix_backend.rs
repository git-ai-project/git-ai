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

    /// List all file paths in a tree recursively (equivalent to `git ls-tree -r --name-only`).
    pub fn ls_tree_recursive(&self, tree_oid_hex: &str) -> Result<Vec<String>, GitAiError> {
        use gix::objs::TreeRefIter;

        let repo = self.get_repo()?;
        let tree_oid = Self::parse_oid(tree_oid_hex)?;

        let mut paths = Vec::new();
        let mut stack: Vec<(String, ObjectId)> = vec![(String::new(), tree_oid)];

        while let Some((prefix, oid)) = stack.pop() {
            let obj = repo
                .find_object(oid)
                .map_err(|e| GitAiError::GixError(format!("find tree '{}': {}", oid, e)))?;
            let data = obj.detach();
            let iter = TreeRefIter::from_bytes(&data.data);
            for entry in iter {
                let entry =
                    entry.map_err(|e| GitAiError::GixError(format!("parse tree entry: {}", e)))?;
                let name = entry.filename.to_string();
                let full_path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", prefix, name)
                };
                if entry.mode.is_tree() {
                    stack.push((full_path, entry.oid.into()));
                } else if entry.mode.is_blob() || entry.mode.is_executable() || entry.mode.is_link()
                {
                    paths.push(full_path);
                }
            }
        }
        paths.sort();
        Ok(paths)
    }

    /// Find refs pointing at a given commit OID.
    pub fn refs_pointing_at(&self, oid_hex: &str) -> Result<Vec<String>, GitAiError> {
        let repo = self.get_repo()?;
        let target_oid = Self::parse_oid(oid_hex)?;
        let mut refs = Vec::new();
        let platform = repo
            .references()
            .map_err(|e| GitAiError::GixError(format!("iterate refs: {}", e)))?;
        for reference in platform
            .all()
            .map_err(|e| GitAiError::GixError(format!("list refs: {}", e)))?
        {
            let reference = match reference {
                Ok(r) => r,
                Err(_) => continue,
            };
            let peeled = match reference.clone().into_fully_peeled_id() {
                Ok(id) => id,
                Err(_) => continue,
            };
            if peeled.as_ref() == target_oid.as_ref() {
                refs.push(reference.name().as_bstr().to_string());
            }
        }
        Ok(refs)
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
    /// Returns commits in newest-first order (like `git rev-list --ancestry-path base..tip`).
    /// Only handles linear histories — returns Err for merge commits so the caller
    /// can fall back to the git CLI which handles ancestry-path filtering correctly.
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
            if info.parent_ids.len() > 1 {
                return Err(GitAiError::GixError(
                    "rev_list: merge commit detected, falling back to CLI".to_string(),
                ));
            }
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

    /// Get full diff deltas between two trees (equivalent to `git diff --raw -z`).
    /// Returns DiffDelta structs with file metadata (path, mode, oid, status).
    pub fn diff_tree_deltas(
        &self,
        from_tree_oid: &str,
        to_tree_oid: &str,
    ) -> Result<Vec<crate::git::diff_tree_to_tree::DiffDelta>, GitAiError> {
        use crate::git::diff_tree_to_tree::{DiffDelta, DiffFile, DiffStatus};
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

        let null_oid = "0000000000000000000000000000000000000000";
        let deltas: Vec<DiffDelta> = recorder
            .records
            .into_iter()
            .map(|change| match change {
                gix::diff::tree::recorder::Change::Addition {
                    path,
                    entry_mode,
                    oid,
                    ..
                } => DiffDelta::new(
                    DiffStatus::Added,
                    DiffFile::new(None, "000000".to_string(), null_oid.to_string()),
                    DiffFile::new(
                        Some(PathBuf::from(path.to_string())),
                        entry_mode.kind().as_octal_str().to_string(),
                        oid.to_string(),
                    ),
                    0,
                ),
                gix::diff::tree::recorder::Change::Deletion {
                    path,
                    entry_mode,
                    oid,
                    ..
                } => DiffDelta::new(
                    DiffStatus::Deleted,
                    DiffFile::new(
                        Some(PathBuf::from(path.to_string())),
                        entry_mode.kind().as_octal_str().to_string(),
                        oid.to_string(),
                    ),
                    DiffFile::new(
                        Some(PathBuf::from(path.to_string())),
                        "000000".to_string(),
                        null_oid.to_string(),
                    ),
                    0,
                ),
                gix::diff::tree::recorder::Change::Modification {
                    path,
                    previous_entry_mode,
                    previous_oid,
                    entry_mode,
                    oid,
                    ..
                } => DiffDelta::new(
                    DiffStatus::Modified,
                    DiffFile::new(
                        Some(PathBuf::from(path.to_string())),
                        previous_entry_mode.kind().as_octal_str().to_string(),
                        previous_oid.to_string(),
                    ),
                    DiffFile::new(
                        Some(PathBuf::from(path.to_string())),
                        entry_mode.kind().as_octal_str().to_string(),
                        oid.to_string(),
                    ),
                    0,
                ),
            })
            .collect();

        Ok(deltas)
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

    /// Compute added line numbers per file between two commits using native
    /// gix blob reads + imara-diff. This avoids spawning `git diff` subprocess.
    ///
    /// Does NOT perform rename detection. For commits involving renames, the
    /// caller should fall back to the git CLI path.
    ///
    /// Returns (added_lines_by_file, total_deleted_lines).
    pub fn diff_added_lines(
        &self,
        from_commit: &str,
        to_commit: &str,
    ) -> Result<(std::collections::HashMap<String, Vec<u32>>, usize), GitAiError> {
        use gix::objs::TreeRefIter;
        use imara_diff::{Algorithm, Diff, InternedInput};
        use std::collections::HashMap;

        let repo = self.get_repo()?;

        let from_tree_oid_str = self.read_commit_tree_oid(from_commit)?;
        let to_tree_oid_str = self.read_commit_tree_oid(to_commit)?;
        let from_tree_oid = Self::parse_oid(&from_tree_oid_str)?;
        let to_tree_oid = Self::parse_oid(&to_tree_oid_str)?;

        let from_obj = repo.find_object(from_tree_oid).map_err(|e| {
            GitAiError::GixError(format!("find tree '{}': {}", from_tree_oid_str, e))
        })?;
        let to_obj = repo
            .find_object(to_tree_oid)
            .map_err(|e| GitAiError::GixError(format!("find tree '{}': {}", to_tree_oid_str, e)))?;

        let from_data = from_obj.detach();
        let to_data = to_obj.detach();
        let from_iter = TreeRefIter::from_bytes(&from_data.data);
        let to_iter = TreeRefIter::from_bytes(&to_data.data);

        let mut recorder = gix::diff::tree::Recorder::default();
        let mut state = gix::diff::tree::State::default();
        (gix::diff::tree)(from_iter, to_iter, &mut state, &repo.objects, &mut recorder)
            .map_err(|e| GitAiError::GixError(format!("diff-tree: {}", e)))?;

        let mut result: HashMap<String, Vec<u32>> = HashMap::new();
        let mut total_deleted: usize = 0;

        for change in recorder.records {
            match change {
                gix::diff::tree::recorder::Change::Addition { path, oid, .. } => {
                    let blob = repo
                        .find_object(oid)
                        .map_err(|e| GitAiError::GixError(format!("read blob: {}", e)))?;
                    let content = String::from_utf8_lossy(&blob.data);
                    let line_count = content.lines().count() as u32;
                    if line_count > 0 {
                        let lines: Vec<u32> = (1..=line_count).collect();
                        result.insert(path.to_string(), lines);
                    }
                }
                gix::diff::tree::recorder::Change::Deletion { oid, .. } => {
                    let blob = repo
                        .find_object(oid)
                        .map_err(|e| GitAiError::GixError(format!("read blob: {}", e)))?;
                    let content = String::from_utf8_lossy(&blob.data);
                    total_deleted += content.lines().count();
                }
                gix::diff::tree::recorder::Change::Modification {
                    path,
                    previous_oid,
                    oid,
                    ..
                } => {
                    let old_blob = repo
                        .find_object(previous_oid)
                        .map_err(|e| GitAiError::GixError(format!("read old blob: {}", e)))?;
                    let new_blob = repo
                        .find_object(oid)
                        .map_err(|e| GitAiError::GixError(format!("read new blob: {}", e)))?;

                    let old_content = String::from_utf8_lossy(&old_blob.data);
                    let new_content = String::from_utf8_lossy(&new_blob.data);

                    let input = InternedInput::new(old_content.as_ref(), new_content.as_ref());
                    let mut diff = Diff::compute(Algorithm::Myers, &input);
                    diff.postprocess_lines(&input);

                    let mut added_lines: Vec<u32> = Vec::new();
                    for hunk in diff.hunks() {
                        let old_count = (hunk.before.end - hunk.before.start) as usize;
                        let new_start = hunk.after.start + 1; // 1-indexed
                        let new_count = hunk.after.end - hunk.after.start;
                        total_deleted += old_count;
                        if new_count > 0 {
                            for line_no in new_start..new_start + new_count {
                                added_lines.push(line_no);
                            }
                        }
                    }
                    if !added_lines.is_empty() {
                        added_lines.sort_unstable();
                        added_lines.dedup();
                        result.insert(path.to_string(), added_lines);
                    }
                }
            }
        }

        Ok((result, total_deleted))
    }

    /// Try native diff_added_lines, returning None on failure.
    pub fn try_diff_added_lines(
        &self,
        from_commit: &str,
        to_commit: &str,
    ) -> Option<(std::collections::HashMap<String, Vec<u32>>, usize)> {
        self.diff_added_lines(from_commit, to_commit).ok()
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

    /// Write note blobs and create a notes commit under refs/notes/ai.
    /// This replaces the `git fast-import` subprocess with native object writes.
    ///
    /// Each entry is (commit_sha, note_content). The notes tree uses fanout
    /// format: `xx/yyyyyyyy...` where xx is the first 2 hex chars.
    pub fn notes_add_batch(&self, entries: &[(String, String)]) -> Result<(), GitAiError> {
        if entries.is_empty() {
            return Ok(());
        }

        let repo = self.get_repo()?;

        // Deduplicate: last entry for each commit wins
        let mut deduped: Vec<(String, String)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (commit_sha, note_content) in entries.iter().rev() {
            if seen.insert(commit_sha.as_str()) {
                deduped.push((commit_sha.clone(), note_content.clone()));
            }
        }
        deduped.reverse();

        // Write note blobs
        let mut blob_ids: Vec<(String, ObjectId)> = Vec::with_capacity(deduped.len());
        for (commit_sha, note_content) in &deduped {
            let blob_id = repo
                .write_blob(note_content.as_bytes())
                .map_err(|e| GitAiError::GixError(format!("write note blob: {}", e)))?;
            blob_ids.push((commit_sha.clone(), blob_id.detach()));
        }

        // Get the existing notes tree (if any)
        let existing_tip: Option<ObjectId> = repo
            .try_find_reference("refs/notes/ai")
            .ok()
            .flatten()
            .and_then(|r| r.into_fully_peeled_id().ok())
            .map(|id| id.detach());

        // Read existing tree entries
        let mut tree_entries: std::collections::BTreeMap<String, (u32, ObjectId)> =
            std::collections::BTreeMap::new();

        if let Some(tip_id) = existing_tip
            && let Ok(commit_obj) = repo.find_object(tip_id)
            && let Ok(commit) = commit_obj.try_into_commit()
            && let Ok(tree_id) = commit.tree_id()
            && let Ok(tree_obj) = repo.find_object(tree_id)
            && let Ok(tree) = tree_obj.try_into_tree()
        {
            self.collect_tree_entries(&repo, &tree, "", &mut tree_entries);
        }

        // Add/update entries for the new notes (using fanout paths)
        for (commit_sha, blob_id) in &blob_ids {
            let fanout_path = if commit_sha.len() > 2 {
                format!("{}/{}", &commit_sha[..2], &commit_sha[2..])
            } else {
                commit_sha.clone()
            };
            // Remove flat path if it exists
            tree_entries.remove(commit_sha);
            // Insert fanout path
            tree_entries.insert(fanout_path, (0o100644, *blob_id));
        }

        // Build the new tree hierarchy
        let new_tree_id = self.build_notes_tree(&repo, &tree_entries)?;

        // Create the notes commit
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| GitAiError::GixError(format!("system clock: {}", e)))?;
        let time_str = format!("{} +0000", now.as_secs());
        let signature = gix::actor::SignatureRef {
            name: b"git-ai".as_slice().into(),
            email: b"git-ai@local".as_slice().into(),
            time: &time_str,
        };

        let parents: Vec<ObjectId> = existing_tip.into_iter().collect();
        repo.commit_as(
            signature,
            signature,
            "refs/notes/ai",
            "",
            new_tree_id,
            parents,
        )
        .map_err(|e| GitAiError::GixError(format!("create notes commit: {}", e)))?;

        Ok(())
    }

    fn collect_tree_entries(
        &self,
        repo: &gix::Repository,
        tree: &gix::Tree<'_>,
        prefix: &str,
        entries: &mut std::collections::BTreeMap<String, (u32, ObjectId)>,
    ) {
        use gix::objs::TreeRefIter;
        let data = tree.data.clone();
        let iter = TreeRefIter::from_bytes(&data);
        for entry in iter {
            let Ok(entry) = entry else { continue };
            let name = entry.filename.to_string();
            let full_path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", prefix, name)
            };
            if entry.mode.is_tree() {
                if let Ok(subtree_obj) = repo.find_object(entry.oid.to_owned())
                    && let Ok(subtree) = subtree_obj.try_into_tree()
                {
                    self.collect_tree_entries(repo, &subtree, &full_path, entries);
                }
            } else {
                let mode_str = entry.mode.kind().as_octal_str().to_string();
                let mode: u32 = u32::from_str_radix(&mode_str, 8).unwrap_or(0o100644);
                entries.insert(full_path, (mode, entry.oid.to_owned()));
            }
        }
    }

    fn build_notes_tree(
        &self,
        repo: &gix::Repository,
        entries: &std::collections::BTreeMap<String, (u32, ObjectId)>,
    ) -> Result<ObjectId, GitAiError> {
        use gix::prelude::Write;
        // Group entries by top-level directory
        let mut top_level: std::collections::BTreeMap<String, Vec<(String, u32, ObjectId)>> =
            std::collections::BTreeMap::new();
        let mut root_entries: Vec<(String, u32, ObjectId)> = Vec::new();

        for (path, (mode, oid)) in entries {
            if let Some(slash_pos) = path.find('/') {
                let dir = &path[..slash_pos];
                let rest = &path[slash_pos + 1..];
                top_level
                    .entry(dir.to_string())
                    .or_default()
                    .push((rest.to_string(), *mode, *oid));
            } else {
                root_entries.push((path.clone(), *mode, *oid));
            }
        }

        // Write subtrees for each directory
        let mut tree_data: Vec<u8> = Vec::new();

        // Entries must be sorted by name for git tree format.
        // Directories come mixed with files in git's byte-sorted order.
        let mut all_entries: Vec<(String, u32, ObjectId)> = Vec::new();

        for (dir_name, dir_entries) in &top_level {
            let subtree_id = self.build_flat_tree(repo, dir_entries)?;
            all_entries.push((dir_name.clone(), 0o040000, subtree_id));
        }
        all_entries.extend(root_entries);

        // Sort by name (git tree format requirement)
        // Git sorts tree entries treating directories as if they have a trailing '/'
        all_entries.sort_by(|a, b| {
            let a_name = if a.1 == 0o040000 {
                format!("{}/", a.0)
            } else {
                a.0.clone()
            };
            let b_name = if b.1 == 0o040000 {
                format!("{}/", b.0)
            } else {
                b.0.clone()
            };
            a_name.cmp(&b_name)
        });

        // Serialize tree object
        for (name, mode, oid) in &all_entries {
            tree_data.extend_from_slice(format!("{:o} {}\0", mode, name).as_bytes());
            tree_data.extend_from_slice(oid.as_bytes());
        }

        let oid = repo
            .objects
            .write_buf(gix::object::Kind::Tree, &tree_data)
            .map_err(|e| GitAiError::GixError(format!("write tree: {}", e)))?;
        Ok(oid)
    }

    fn build_flat_tree(
        &self,
        repo: &gix::Repository,
        entries: &[(String, u32, ObjectId)],
    ) -> Result<ObjectId, GitAiError> {
        use gix::prelude::Write;
        let mut sorted_entries = entries.to_vec();
        sorted_entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut tree_data: Vec<u8> = Vec::new();
        for (name, mode, oid) in &sorted_entries {
            tree_data.extend_from_slice(format!("{:o} {}\0", mode, name).as_bytes());
            tree_data.extend_from_slice(oid.as_bytes());
        }

        let oid = repo
            .objects
            .write_buf(gix::object::Kind::Tree, &tree_data)
            .map_err(|e| GitAiError::GixError(format!("write subtree: {}", e)))?;
        Ok(oid)
    }
}
