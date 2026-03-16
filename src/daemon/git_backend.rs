use crate::daemon::domain::{FamilyKey, RefChange, RepoContext};
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::repository::exec_git_allow_nonzero;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReflogCut {
    pub offsets: HashMap<String, u64>,
}

pub trait GitBackend: Send + Sync + 'static {
    fn resolve_family(&self, worktree: &Path) -> Result<FamilyKey, GitAiError>;

    fn repo_context(&self, worktree: &Path) -> Result<RepoContext, GitAiError>;

    fn ref_snapshot(&self, family: &FamilyKey) -> Result<HashMap<String, String>, GitAiError>;

    fn reflog_cut(&self, family: &FamilyKey) -> Result<ReflogCut, GitAiError>;

    fn reflog_delta(
        &self,
        family: &FamilyKey,
        start: &ReflogCut,
        end: &ReflogCut,
    ) -> Result<Vec<RefChange>, GitAiError>;

    fn clone_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf>;

    fn init_target(&self, argv: &[String], cwd_hint: Option<&Path>) -> Option<PathBuf>;
}

#[derive(Debug, Default)]
pub struct SystemGitBackend;

impl SystemGitBackend {
    pub fn new() -> Self {
        Self::default()
    }
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

    fn ref_snapshot(&self, family: &FamilyKey) -> Result<HashMap<String, String>, GitAiError> {
        let git_dir = PathBuf::from(&family.0);
        if !git_dir.exists() {
            return Err(GitAiError::Generic(format!(
                "family common_dir does not exist: {}",
                family.0
            )));
        }

        let output = run_git_allow_nonzero(
            [
                "--git-dir",
                &family.0,
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
            ]
            .as_slice(),
        )?;
        if !output.status.success() {
            return Err(git_error_for(
                [
                    "--git-dir",
                    &family.0,
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)",
                ]
                .as_slice(),
                &output,
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut refs = HashMap::new();
        for line in stdout.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(2, '\0');
            let reference = parts.next().unwrap_or_default().trim();
            let oid = parts.next().unwrap_or_default().trim();
            if reference.is_empty() || oid.is_empty() {
                continue;
            }
            refs.insert(reference.to_string(), oid.to_string());
        }

        if let Ok(head_oid) = run_git_str_allow_nonzero(
            ["--git-dir", &family.0, "rev-parse", "--verify", "HEAD"].as_slice(),
        ) && !head_oid.is_empty()
        {
            refs.insert("HEAD".to_string(), head_oid);
        }

        Ok(refs)
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
        if reference == "HEAD" || reference.starts_with("refs/") {
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
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
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

fn command_args(argv: &[String], command: &str) -> Vec<String> {
    let slice: &[String] = if argv.first().map(|v| is_git_binary(v)).unwrap_or(false) {
        &argv[1..]
    } else {
        argv
    };
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
