use crate::daemon::domain::{FamilyKey, RefChange, RepoContext};
use crate::error::GitAiError;
use crate::git::cli_parser::parse_git_cli_args;
use crate::git::find_repository_in_path;
use crate::git::repo_state::common_dir_for_worktree;
use crate::git::repository::discover_repository_in_path_no_git_exec;
use crate::git::repository::exec_git_allow_nonzero;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReflogCut {
    pub offsets: HashMap<String, u64>,
}

pub trait GitBackend: Send + Sync + 'static {
    fn resolve_family(&self, worktree: &Path) -> Result<FamilyKey, GitAiError>;

    fn repo_context(&self, worktree: &Path) -> Result<RepoContext, GitAiError>;

    fn reflog_cut(&self, family: &FamilyKey) -> Result<ReflogCut, GitAiError>;

    fn reflog_delta(
        &self,
        family: &FamilyKey,
        start: &ReflogCut,
        end: &ReflogCut,
    ) -> Result<Vec<RefChange>, GitAiError>;

    fn resolve_primary_command(
        &self,
        worktree: &Path,
        argv: &[String],
    ) -> Result<Option<String>, GitAiError>;

    fn clone_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf>;

    fn init_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf>;
}

const ALIAS_CACHE_TTL_SECS: u64 = 60;

struct AliasCacheEntry {
    /// Resolved alias name → expansion value (e.g. "ci" → "commit -v")
    aliases: HashMap<String, String>,
    refreshed_at: Instant,
    /// Set to true while a background thread is refreshing this entry,
    /// preventing thundering-herd spawns when many events arrive after TTL.
    refresh_in_progress: bool,
}

pub struct SystemGitBackend {
    alias_cache: Arc<Mutex<HashMap<String, AliasCacheEntry>>>,
}

impl std::fmt::Debug for SystemGitBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemGitBackend").finish()
    }
}

impl Default for SystemGitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemGitBackend {
    pub fn new() -> Self {
        Self {
            alias_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Look up a single alias from the per-family cache.
    ///
    /// Uses stale-while-revalidate: if the cache entry is expired, the stale
    /// value is returned immediately and a background thread refreshes it.
    /// This ensures alias resolution is never on the critical path.
    fn resolve_alias_cached(
        &self,
        worktree: &Path,
        alias_name: &str,
    ) -> Result<Option<String>, GitAiError> {
        let family_key = match common_dir_for_worktree(worktree) {
            Some(common_dir) => common_dir
                .canonicalize()
                .unwrap_or(common_dir)
                .to_string_lossy()
                .to_string(),
            None => return self.resolve_alias_uncached(worktree, alias_name),
        };

        let cache = self
            .alias_cache
            .lock()
            .map_err(|_| GitAiError::Generic("alias cache lock poisoned".to_string()))?;

        if let Some(entry) = cache.get(&family_key) {
            let normalized_alias = alias_name.to_ascii_lowercase();
            let result = entry.aliases.get(&normalized_alias).cloned();
            if entry.refreshed_at.elapsed().as_secs() >= ALIAS_CACHE_TTL_SECS
                && !entry.refresh_in_progress
            {
                // Stale — return cached value but kick off background refresh.
                // Mark in-progress to prevent thundering-herd thread spawns.
                drop(cache);
                if let Ok(mut cache) = self.alias_cache.lock()
                    && let Some(entry) = cache.get_mut(&family_key)
                {
                    entry.refresh_in_progress = true;
                }
                let worktree = worktree.to_path_buf();
                let family_key = family_key.clone();
                let alias_cache = self.alias_cache.clone();
                std::thread::spawn(move || {
                    refresh_alias_cache(&worktree, &family_key, &alias_cache);
                });
            }
            return Ok(result);
        }
        drop(cache);

        // Cold miss — must load synchronously for the first call.
        // If the sync refresh fails (e.g. repo discovery error), the cache won't
        // contain the family key. Fall back to uncached resolution which correctly
        // propagates errors.
        self.refresh_alias_cache_sync(worktree, &family_key)?;
        let cache = self
            .alias_cache
            .lock()
            .map_err(|_| GitAiError::Generic("alias cache lock poisoned".to_string()))?;
        match cache.get(&family_key) {
            Some(entry) => {
                let normalized_alias = alias_name.to_ascii_lowercase();
                Ok(entry.aliases.get(&normalized_alias).cloned())
            }
            None => {
                drop(cache);
                self.resolve_alias_uncached(worktree, alias_name)
            }
        }
    }

    /// Synchronously load all aliases for a family into the cache.
    fn refresh_alias_cache_sync(
        &self,
        worktree: &Path,
        family_key: &str,
    ) -> Result<(), GitAiError> {
        refresh_alias_cache(worktree, family_key, &self.alias_cache);
        Ok(())
    }

    /// Fallback when we can't determine a family key for caching.
    fn resolve_alias_uncached(
        &self,
        worktree: &Path,
        alias_name: &str,
    ) -> Result<Option<String>, GitAiError> {
        let repo = discover_repository_in_path_no_git_exec(worktree)?;
        let key = format!("alias.{}", alias_name);
        repo.config_get_str(&key)
    }
}

/// Load aliases from disk and store them in the cache. Safe to call from any
/// thread — errors are silently swallowed when running as a background refresh.
fn refresh_alias_cache(
    worktree: &Path,
    family_key: &str,
    alias_cache: &Mutex<HashMap<String, AliasCacheEntry>>,
) {
    let aliases = match discover_repository_in_path_no_git_exec(worktree).and_then(|repo| {
        repo.get_git_config_file()
            .map(|cfg| read_all_aliases_from_config(&cfg))
    }) {
        Ok(aliases) => aliases,
        Err(_) => {
            // Clear refresh_in_progress so a future attempt can retry.
            if let Ok(mut cache) = alias_cache.lock()
                && let Some(entry) = cache.get_mut(family_key)
            {
                entry.refresh_in_progress = false;
            }
            return;
        }
    };
    if let Ok(mut cache) = alias_cache.lock() {
        cache.insert(
            family_key.to_string(),
            AliasCacheEntry {
                aliases,
                refreshed_at: Instant::now(),
                refresh_in_progress: false,
            },
        );
    }
}

fn read_all_aliases_from_config(config: &gix_config::File<'_>) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let Some(sections) = config.sections_by_name("alias") else {
        return aliases;
    };
    for section in sections {
        let body = section.body();
        for key in body.value_names() {
            let key_str = key.to_string();
            if key_str.is_empty() {
                continue;
            }
            if let Some(value) = body.value(&key_str) {
                aliases.insert(key_str.to_ascii_lowercase(), value.to_string());
            }
        }
    }
    aliases
}

fn is_builtin_primary_command(command: &str) -> bool {
    matches!(
        command,
        "add"
            | "blame"
            | "branch"
            | "cat-file"
            | "check-attr"
            | "check-ignore"
            | "check-mailmap"
            | "checkout"
            | "cherry-pick"
            | "clean"
            | "clone"
            | "commit"
            | "config"
            | "count-objects"
            | "describe"
            | "diff"
            | "diff-files"
            | "diff-index"
            | "diff-tree"
            | "fetch"
            | "for-each-ref"
            | "grep"
            | "hash-object"
            | "help"
            | "init"
            | "log"
            | "ls-files"
            | "ls-tree"
            | "merge"
            | "merge-base"
            | "mktree"
            | "mv"
            | "name-rev"
            | "notes"
            | "pull"
            | "push"
            | "rebase"
            | "remote"
            | "reset"
            | "restore"
            | "rev-list"
            | "rev-parse"
            | "revert"
            | "rm"
            | "shortlog"
            | "show"
            | "stash"
            | "status"
            | "switch"
            | "symbolic-ref"
            | "tag"
            | "update-ref"
            | "var"
            | "verify-commit"
            | "verify-tag"
            | "version"
            | "worktree"
    )
}

impl GitBackend for SystemGitBackend {
    fn resolve_family(&self, worktree: &Path) -> Result<FamilyKey, GitAiError> {
        let worktree_str = worktree.to_string_lossy().to_string();
        let repo = find_repository_in_path(&worktree_str)?;
        let common = repo
            .common_dir()
            .canonicalize()
            .unwrap_or_else(|_| repo.common_dir().to_path_buf());
        Ok(FamilyKey::new(common.to_string_lossy().to_string()))
    }

    fn repo_context(&self, worktree: &Path) -> Result<RepoContext, GitAiError> {
        let head = rev_parse_head(worktree).ok();
        let symbolic = run_git_allow_nonzero(
            [
                "-C",
                &worktree.to_string_lossy(),
                "symbolic-ref",
                "--quiet",
                "--short",
                "HEAD",
            ]
            .as_slice(),
        )?;
        let (branch, detached) = if symbolic.status.success() {
            let value = String::from_utf8_lossy(&symbolic.stdout).trim().to_string();
            if value.is_empty() {
                (None, true)
            } else {
                (Some(value), false)
            }
        } else {
            (None, true)
        };

        Ok(RepoContext {
            head,
            branch,
            detached,
        })
    }

    fn reflog_cut(&self, family: &FamilyKey) -> Result<ReflogCut, GitAiError> {
        let common_dir = PathBuf::from(&family.0);
        let offsets = reflog_offsets(&common_dir)?;
        Ok(ReflogCut { offsets })
    }

    fn reflog_delta(
        &self,
        family: &FamilyKey,
        start: &ReflogCut,
        end: &ReflogCut,
    ) -> Result<Vec<RefChange>, GitAiError> {
        let common_dir = PathBuf::from(&family.0);
        let refs = start
            .offsets
            .keys()
            .chain(end.offsets.keys())
            .cloned()
            .collect::<HashSet<_>>();

        let mut changes = Vec::new();
        for reference in refs {
            let start_offset = start.offsets.get(&reference).copied().unwrap_or(0);
            let end_offset = end.offsets.get(&reference).copied().unwrap_or(start_offset);
            if end_offset < start_offset {
                return Err(GitAiError::Generic(format!(
                    "reflog cut regressed for {} ({} < {})",
                    reference, end_offset, start_offset
                )));
            }
            if end_offset == start_offset {
                continue;
            }

            let reflog_path = common_dir.join("logs").join(&reference);
            if !reflog_path.exists() {
                return Err(GitAiError::Generic(format!(
                    "reflog path missing for {}: {}",
                    reference,
                    reflog_path.display()
                )));
            }

            let metadata = fs::metadata(&reflog_path)?;
            let file_len = metadata.len();
            if file_len < end_offset {
                return Err(GitAiError::Generic(format!(
                    "reflog shorter than cut for {} ({} < {})",
                    reference, file_len, end_offset
                )));
            }

            let mut file = File::open(&reflog_path)?;
            file.seek(SeekFrom::Start(start_offset))?;
            let take_len = end_offset.saturating_sub(start_offset);
            let reader = BufReader::new(file.take(take_len));
            for line in reader.lines() {
                let line = line?;
                if let Some(change) = parse_reflog_line(&reference, &line) {
                    changes.push(change);
                }
            }
        }

        Ok(changes)
    }

    fn resolve_primary_command(
        &self,
        worktree: &Path,
        argv: &[String],
    ) -> Result<Option<String>, GitAiError> {
        let mut current = parse_git_cli_args(git_invocation_tokens(argv));
        let mut seen = HashSet::new();
        loop {
            let Some(command) = current.command.clone() else {
                return Ok(None);
            };
            if !seen.insert(command.clone()) {
                return Ok(None);
            }
            if is_builtin_primary_command(&command) {
                return Ok(Some(command));
            }

            let alias_value = match self.resolve_alias_cached(worktree, &command)? {
                Some(value) => value,
                None => return Ok(Some(command)),
            };

            let Some(alias_tokens) = parse_alias_tokens(&alias_value) else {
                return Ok(None);
            };

            let mut expanded_args = Vec::new();
            expanded_args.extend(current.global_args.iter().cloned());
            expanded_args.extend(alias_tokens);
            expanded_args.extend(current.command_args.iter().cloned());
            current = parse_git_cli_args(&expanded_args);
        }
    }

    fn clone_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf> {
        let args = command_args(argv, "clone");
        let positional = clone_init_positionals(&args);
        if positional.is_empty() {
            return None;
        }
        let target = if positional.len() >= 2 {
            PathBuf::from(&positional[1])
        } else {
            default_clone_target_from_source(&positional[0])?
        };
        Some(resolve_target(target, cwd_hint))
    }

    fn init_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf> {
        let args = command_args(argv, "init");
        let positional = clone_init_positionals(&args);
        let target = positional
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Some(resolve_target(target, cwd_hint))
    }
}

fn rev_parse_head(worktree: &Path) -> Result<String, GitAiError> {
    run_git_str_allow_nonzero(
        [
            "-C",
            &worktree.to_string_lossy(),
            "rev-parse",
            "--verify",
            "HEAD",
        ]
        .as_slice(),
    )
}

fn run_git_allow_nonzero(args: &[&str]) -> Result<std::process::Output, GitAiError> {
    let args_owned = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    exec_git_allow_nonzero(&args_owned)
}

fn run_git_str_allow_nonzero(args: &[&str]) -> Result<String, GitAiError> {
    let output = run_git_allow_nonzero(args)?;
    if !output.status.success() {
        return Err(git_error_for(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_error_for(args: &[&str], output: &std::process::Output) -> GitAiError {
    GitAiError::GitCliError {
        code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    }
}

fn reflog_offsets(common_dir: &Path) -> Result<HashMap<String, u64>, GitAiError> {
    let mut out = HashMap::new();
    let logs_dir = common_dir.join("logs");
    if !logs_dir.exists() {
        return Ok(out);
    }
    discover_reflog_files(&logs_dir, &logs_dir, &mut out)?;
    Ok(out)
}

fn discover_reflog_files(
    root: &Path,
    current: &Path,
    out: &mut HashMap<String, u64>,
) -> Result<(), GitAiError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            discover_reflog_files(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let relative = match path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(_) => continue,
        };
        let reference = relative.to_string_lossy().replace('\\', "/");
        if reference == "HEAD" || reference == "ORIG_HEAD" || reference.starts_with("refs/") {
            let offset = fs::metadata(&path)?.len();
            out.insert(reference, offset);
        }
    }
    Ok(())
}

fn parse_reflog_line(reference: &str, line: &str) -> Option<RefChange> {
    let head = line.split('\t').next().unwrap_or_default();
    let mut parts = head.split_whitespace();
    let old = parts.next()?.trim().to_string();
    let new = parts.next()?.trim().to_string();
    if !is_valid_oid(&old) || !is_valid_oid(&new) || old == new {
        return None;
    }
    Some(RefChange {
        reference: reference.to_string(),
        old,
        new,
    })
}

fn is_valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_git_binary(token: &str) -> bool {
    if token == "git" || token == "git.exe" {
        return true;
    }
    Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name == "git" || name == "git.exe")
        .unwrap_or(false)
}

fn git_invocation_tokens(argv: &[String]) -> &[String] {
    if argv
        .first()
        .map(|token| is_git_binary(token))
        .unwrap_or(false)
    {
        &argv[1..]
    } else {
        argv
    }
}

fn command_args(argv: &[String], command: &str) -> Vec<String> {
    let slice = git_invocation_tokens(argv);
    let mut seen = false;
    let mut out = Vec::new();
    for token in slice {
        if !seen {
            if token == command {
                seen = true;
            }
            continue;
        }
        out.push(token.clone());
    }
    out
}

fn clone_init_positionals(args: &[String]) -> Vec<String> {
    let mut positionals = Vec::new();
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--" {
            positionals.extend(args[idx + 1..].iter().cloned());
            break;
        }
        if arg.starts_with('-') {
            if takes_value(arg) && idx + 1 < args.len() {
                idx += 2;
                continue;
            }
            idx += 1;
            continue;
        }
        positionals.push(arg.clone());
        idx += 1;
    }
    positionals
}

fn takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-b" | "--branch"
            | "--origin"
            | "--upload-pack"
            | "--template"
            | "--separate-git-dir"
            | "--reference"
            | "--dissociate"
            | "--config"
            | "--object-format"
    )
}

fn default_clone_target_from_source(source: &str) -> Option<PathBuf> {
    let source = source.trim_end_matches('/');
    let source = source.strip_suffix(".git").unwrap_or(source);
    let name = source.rsplit('/').next()?.rsplit(':').next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(PathBuf::from(name))
}

fn resolve_target(target: PathBuf, cwd_hint: Option<&Path>) -> PathBuf {
    if target.is_absolute() {
        return target;
    }
    if let Some(cwd) = cwd_hint {
        return cwd.join(target);
    }
    target
}

fn parse_alias_tokens(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim_start();
    if trimmed.starts_with('!') {
        return None;
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in trimmed.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        if in_double {
            match ch {
                '"' => in_double = false,
                '\\' => escaped = true,
                _ => current.push(ch),
            }
            continue;
        }

        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '\\' => escaped = true,
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }
    if in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::{GitBackend, SystemGitBackend};
    use std::path::PathBuf;

    #[test]
    fn builtin_primary_command_skips_repository_lookup() {
        let backend = SystemGitBackend::new();
        let missing_worktree = PathBuf::from("/definitely/missing/git-ai-backend-test");
        let argv = vec!["git".to_string(), "commit".to_string()];

        let resolved = backend
            .resolve_primary_command(&missing_worktree, &argv)
            .expect("builtin commands should not require repository discovery");

        assert_eq!(resolved.as_deref(), Some("commit"));
    }

    #[test]
    fn unknown_primary_command_still_requires_repository_lookup() {
        let backend = SystemGitBackend::new();
        let missing_worktree = PathBuf::from("/definitely/missing/git-ai-backend-test");
        let argv = vec!["git".to_string(), "ci".to_string()];

        assert!(
            backend
                .resolve_primary_command(&missing_worktree, &argv)
                .is_err(),
            "unknown commands should still consult repository alias config"
        );
    }
}
