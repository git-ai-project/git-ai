use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gix::bstr::BStr;
use gix::ObjectId;

use crate::error::GitAiError;

/// Backend that uses `gix` for native git object reads (both loose and packed).
///
/// This replaces the loose-object-only FastReader approach with full packfile
/// support, eliminating the need to spawn `git` subprocesses for packed objects.
pub struct GixBackend {
    git_dir: PathBuf,
    repo: OnceLock<gix::ThreadSafeRepository>,
}

impl GixBackend {
    pub fn new(git_dir: &Path) -> Self {
        Self {
            git_dir: git_dir.to_path_buf(),
            repo: OnceLock::new(),
        }
    }

    fn get_repo(&self) -> Result<gix::Repository, GitAiError> {
        let ts_repo = self.repo.get_or_init(|| {
            let opts = gix::open::Options::isolated();
            gix::open_opts(&self.git_dir, opts).expect("failed to open git repository with gix").into()
        });
        Ok(ts_repo.to_thread_local())
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
        Ok(commit.tree_id().map_err(|e| {
            GitAiError::GixError(format!("read tree from commit '{}': {}", commit_oid_hex, e))
        })?.to_string())
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
        let reference = repo.find_reference(refname).map_err(|e| {
            GitAiError::GixError(format!("find reference '{}': {}", refname, e))
        })?;
        let oid = reference.into_fully_peeled_id().map_err(|e| {
            GitAiError::GixError(format!("peel reference '{}': {}", refname, e))
        })?;
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
        let parent_ids: Vec<String> = commit
            .parent_ids()
            .map(|id| id.to_string())
            .collect();
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
}
