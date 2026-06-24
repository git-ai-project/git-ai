use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub(crate) enum BashHintConfidence {
    Weak,
    Medium,
    Strong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BashRepoHint {
    pub repo_work_dir: PathBuf,
    pub target_paths: Vec<PathBuf>,
    pub confidence: BashHintConfidence,
    pub evidence: String,
}

pub(crate) struct BashAttemptView<'a> {
    pub original_cwd: &'a Path,
    pub discovered_repo_work_dir: Option<&'a Path>,
    pub command: Option<&'a str>,
}

pub(crate) struct RecoveryContext<'a> {
    pub target_repo_work_dir: &'a Path,
}

pub(crate) trait BashRepoHintStrategy {
    fn name(&self) -> &'static str;
    fn infer(&self, attempt: &BashAttemptView<'_>, ctx: &RecoveryContext<'_>) -> Vec<BashRepoHint>;
}

pub(crate) fn infer_bash_repo_hints(
    attempt: &BashAttemptView<'_>,
    ctx: &RecoveryContext<'_>,
) -> Vec<BashRepoHint> {
    let strategies: [&dyn BashRepoHintStrategy; 6] = [
        &OriginalCwdRepoStrategy,
        &AbsolutePathStrategy,
        &LeadingCdStrategy,
        &RedirectionPathStrategy,
        &GitCStrategy,
        &ToolCwdFlagStrategy,
    ];

    let mut hints = Vec::new();
    for strategy in strategies {
        hints.extend(strategy.infer(attempt, ctx));
    }
    dedup_hints(hints)
}

pub(crate) fn normalize_path_for_matching(path: &Path) -> PathBuf {
    let lexical = normalize_components(path);
    let path = lexical.as_path();

    if let Ok(canonical) = path.canonicalize() {
        return normalize_components(&canonical);
    }

    let mut existing = path;
    let mut missing = Vec::<OsString>::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        missing.push(name.to_os_string());
        let Some(parent) = existing.parent() else {
            break;
        };
        if parent == existing {
            break;
        }
        existing = parent;
    }

    let mut rebuilt = existing
        .canonicalize()
        .unwrap_or_else(|_| existing.to_path_buf());
    for part in missing.into_iter().rev() {
        rebuilt.push(part);
    }
    normalize_components(&rebuilt)
}

pub(crate) fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = normalize_path_for_matching(path);
    let root = normalize_path_for_matching(root);
    path == root || path.starts_with(root)
}

fn dedup_hints(hints: Vec<BashRepoHint>) -> Vec<BashRepoHint> {
    let mut deduped = Vec::<BashRepoHint>::new();
    for hint in hints {
        if !deduped.iter().any(|existing| {
            existing.repo_work_dir == hint.repo_work_dir
                && existing.target_paths == hint.target_paths
                && existing.confidence == hint.confidence
                && existing.evidence == hint.evidence
        }) {
            deduped.push(hint);
        }
    }
    deduped
}

fn normalize_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn repo_hint(
    target_repo: &Path,
    target_paths: Vec<PathBuf>,
    confidence: BashHintConfidence,
    evidence: impl Into<String>,
) -> BashRepoHint {
    BashRepoHint {
        repo_work_dir: normalize_path_for_matching(target_repo),
        target_paths: target_paths
            .into_iter()
            .map(|path| normalize_path_for_matching(&path))
            .collect(),
        confidence,
        evidence: evidence.into(),
    }
}

fn hint_for_candidate_path(
    path: &Path,
    target_repo: &Path,
    evidence: impl Into<String>,
) -> Option<BashRepoHint> {
    if !path_is_within(path, target_repo) {
        return None;
    }
    Some(repo_hint(
        target_repo,
        vec![path.to_path_buf()],
        BashHintConfidence::Strong,
        evidence,
    ))
}

fn resolve_path(base: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn command_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else if ch == '\\' && active_quote == '"' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ch if ch.is_whitespace() => {
                push_token(&mut tokens, &mut current);
            }
            ';' | '(' | ')' | '{' | '}' => {
                push_token(&mut tokens, &mut current);
                tokens.push(ch.to_string());
            }
            '&' | '|' => {
                push_token(&mut tokens, &mut current);
                if chars.peek() == Some(&ch) {
                    chars.next();
                    tokens.push(format!("{ch}{ch}"));
                } else {
                    tokens.push(ch.to_string());
                }
            }
            '>' | '<' => {
                push_token(&mut tokens, &mut current);
                let mut op = ch.to_string();
                while chars.peek() == Some(&ch) {
                    chars.next();
                    op.push(ch);
                }
                tokens.push(op);
            }
            _ => current.push(ch),
        }
    }
    push_token(&mut tokens, &mut current);
    tokens
}

fn push_token(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

fn redirection_target_tokens(tokens: &[String]) -> Vec<&str> {
    let mut paths = Vec::new();
    for index in 0..tokens.len() {
        let token = tokens[index].as_str();
        let next = tokens.get(index + 1).map(String::as_str);
        if matches!(token, ">" | ">>" | "<" | "<<" | "<<<")
            && let Some(path) = next
            && !is_control_token(path)
        {
            paths.push(path);
        }
        if token.ends_with('>')
            && token.len() > 1
            && token.chars().all(|ch| ch.is_ascii_digit() || ch == '>')
            && let Some(path) = next
            && !is_control_token(path)
        {
            paths.push(path);
        }
        if token == "tee" {
            let mut cursor = index + 1;
            while let Some(candidate) = tokens.get(cursor).map(String::as_str) {
                if candidate.starts_with('-') {
                    cursor += 1;
                    continue;
                }
                if !is_control_token(candidate) {
                    paths.push(candidate);
                }
                break;
            }
        }
    }
    paths
}

fn is_control_token(token: &str) -> bool {
    matches!(token, ";" | "&&" | "||" | "|" | "(" | ")" | "{" | "}")
}

fn leading_effective_cwd(tokens: &[String], original_cwd: &Path) -> Option<PathBuf> {
    let mut index = 0;
    while matches!(tokens.get(index).map(String::as_str), Some("(" | "{")) {
        index += 1;
    }

    match tokens.get(index).map(String::as_str) {
        Some("cd") => {
            let dir = tokens.get(index + 1)?;
            if is_control_token(dir) {
                return None;
            }
            Some(resolve_path(original_cwd, dir))
        }
        Some("pushd") => {
            let dir = tokens.get(index + 1)?;
            if is_control_token(dir) {
                return None;
            }
            Some(resolve_path(original_cwd, dir))
        }
        _ => None,
    }
}

struct OriginalCwdRepoStrategy;

impl BashRepoHintStrategy for OriginalCwdRepoStrategy {
    fn name(&self) -> &'static str {
        "original_cwd_repo"
    }

    fn infer(&self, attempt: &BashAttemptView<'_>, ctx: &RecoveryContext<'_>) -> Vec<BashRepoHint> {
        let mut hints = Vec::new();
        if let Some(discovered_repo) = attempt.discovered_repo_work_dir
            && normalize_path_for_matching(discovered_repo)
                == normalize_path_for_matching(ctx.target_repo_work_dir)
        {
            hints.push(repo_hint(
                ctx.target_repo_work_dir,
                Vec::new(),
                BashHintConfidence::Strong,
                self.name(),
            ));
        } else if path_is_within(attempt.original_cwd, ctx.target_repo_work_dir) {
            hints.push(repo_hint(
                ctx.target_repo_work_dir,
                Vec::new(),
                BashHintConfidence::Strong,
                self.name(),
            ));
        }
        hints
    }
}

struct AbsolutePathStrategy;

impl BashRepoHintStrategy for AbsolutePathStrategy {
    fn name(&self) -> &'static str {
        "absolute_path"
    }

    fn infer(&self, attempt: &BashAttemptView<'_>, ctx: &RecoveryContext<'_>) -> Vec<BashRepoHint> {
        let Some(command) = attempt.command else {
            return Vec::new();
        };
        command_tokens(command)
            .into_iter()
            .filter(|token| Path::new(token).is_absolute())
            .filter_map(|token| {
                hint_for_candidate_path(Path::new(&token), ctx.target_repo_work_dir, self.name())
            })
            .collect()
    }
}

struct LeadingCdStrategy;

impl BashRepoHintStrategy for LeadingCdStrategy {
    fn name(&self) -> &'static str {
        "leading_cd"
    }

    fn infer(&self, attempt: &BashAttemptView<'_>, ctx: &RecoveryContext<'_>) -> Vec<BashRepoHint> {
        let Some(command) = attempt.command else {
            return Vec::new();
        };
        let tokens = command_tokens(command);
        let Some(effective_cwd) = leading_effective_cwd(&tokens, attempt.original_cwd) else {
            return Vec::new();
        };
        if !path_is_within(&effective_cwd, ctx.target_repo_work_dir) {
            return Vec::new();
        }

        let redirection_targets = redirection_target_tokens(&tokens);
        if redirection_targets.is_empty() {
            return vec![repo_hint(
                ctx.target_repo_work_dir,
                Vec::new(),
                BashHintConfidence::Strong,
                self.name(),
            )];
        }
        redirection_targets
            .into_iter()
            .filter_map(|target| {
                let resolved = resolve_path(&effective_cwd, target);
                hint_for_candidate_path(&resolved, ctx.target_repo_work_dir, self.name())
            })
            .collect()
    }
}

struct RedirectionPathStrategy;

impl BashRepoHintStrategy for RedirectionPathStrategy {
    fn name(&self) -> &'static str {
        "redirection_path"
    }

    fn infer(&self, attempt: &BashAttemptView<'_>, ctx: &RecoveryContext<'_>) -> Vec<BashRepoHint> {
        let Some(command) = attempt.command else {
            return Vec::new();
        };
        let tokens = command_tokens(command);
        redirection_target_tokens(&tokens)
            .into_iter()
            .filter_map(|target| {
                let resolved = resolve_path(attempt.original_cwd, target);
                hint_for_candidate_path(&resolved, ctx.target_repo_work_dir, self.name())
            })
            .collect()
    }
}

struct GitCStrategy;

impl BashRepoHintStrategy for GitCStrategy {
    fn name(&self) -> &'static str {
        "git_c"
    }

    fn infer(&self, attempt: &BashAttemptView<'_>, ctx: &RecoveryContext<'_>) -> Vec<BashRepoHint> {
        let Some(command) = attempt.command else {
            return Vec::new();
        };
        let tokens = command_tokens(command);
        let mut hints = Vec::new();
        for index in 0..tokens.len() {
            if tokens[index] == "git"
                && tokens.get(index + 1).map(String::as_str) == Some("-C")
                && let Some(dir) = tokens.get(index + 2)
            {
                let resolved = resolve_path(attempt.original_cwd, dir);
                if path_is_within(&resolved, ctx.target_repo_work_dir) {
                    hints.push(repo_hint(
                        ctx.target_repo_work_dir,
                        Vec::new(),
                        BashHintConfidence::Strong,
                        self.name(),
                    ));
                }
            }
        }
        hints
    }
}

struct ToolCwdFlagStrategy;

impl BashRepoHintStrategy for ToolCwdFlagStrategy {
    fn name(&self) -> &'static str {
        "tool_cwd_flag"
    }

    fn infer(&self, attempt: &BashAttemptView<'_>, ctx: &RecoveryContext<'_>) -> Vec<BashRepoHint> {
        let Some(command) = attempt.command else {
            return Vec::new();
        };
        let tokens = command_tokens(command);
        let mut hints = Vec::new();
        for index in 0..tokens.len() {
            let flag_dir = match tokens[index].as_str() {
                "make" if tokens.get(index + 1).map(String::as_str) == Some("-C") => {
                    tokens.get(index + 2)
                }
                "npm" if tokens.get(index + 1).map(String::as_str) == Some("--prefix") => {
                    tokens.get(index + 2)
                }
                "yarn" if tokens.get(index + 1).map(String::as_str) == Some("--cwd") => {
                    tokens.get(index + 2)
                }
                "pnpm" if tokens.get(index + 1).map(String::as_str) == Some("-C") => {
                    tokens.get(index + 2)
                }
                _ => None,
            };
            if let Some(dir) = flag_dir {
                let resolved = resolve_path(attempt.original_cwd, dir);
                if path_is_within(&resolved, ctx.target_repo_work_dir) {
                    hints.push(repo_hint(
                        ctx.target_repo_work_dir,
                        Vec::new(),
                        BashHintConfidence::Strong,
                        self.name(),
                    ));
                }
            }
        }
        hints
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(repo: &Path) -> RecoveryContext<'_> {
        RecoveryContext {
            target_repo_work_dir: repo,
        }
    }

    fn attempt<'a>(cwd: &'a Path, command: &'a str) -> BashAttemptView<'a> {
        BashAttemptView {
            original_cwd: cwd,
            discovered_repo_work_dir: None,
            command: Some(command),
        }
    }

    #[test]
    fn absolute_path_inside_repo_is_strong_file_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("workspace/project");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let command = format!("printf x >> {}", repo.join("src/a.rs").display());

        let hints = infer_bash_repo_hints(&attempt(tmp.path(), &command), &ctx(&repo));

        assert!(hints.iter().any(|hint| {
            hint.confidence == BashHintConfidence::Strong
                && hint.target_paths == vec![normalize_path_for_matching(&repo.join("src/a.rs"))]
        }));
    }

    #[test]
    fn absolute_path_with_parent_component_normalizes_inside_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("workspace/project");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let command = format!("printf x >> {}", repo.join("src/toto/../a.rs").display());

        let hints = infer_bash_repo_hints(&attempt(tmp.path(), &command), &ctx(&repo));

        assert!(hints.iter().any(|hint| {
            hint.confidence == BashHintConfidence::Strong
                && hint.target_paths == vec![normalize_path_for_matching(&repo.join("src/a.rs"))]
        }));
    }

    #[test]
    fn leading_cd_resolves_relative_redirection_into_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let repo = workspace.join("project");
        std::fs::create_dir_all(repo.join("src")).unwrap();

        let hints = infer_bash_repo_hints(
            &attempt(&workspace, "cd project && printf x >> src/a.rs"),
            &ctx(&repo),
        );

        assert!(hints.iter().any(|hint| {
            hint.confidence == BashHintConfidence::Strong
                && hint.target_paths == vec![normalize_path_for_matching(&repo.join("src/a.rs"))]
        }));
    }

    #[test]
    fn subshell_cd_resolves_relative_redirection_into_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let repo = workspace.join("project");
        std::fs::create_dir_all(repo.join("src")).unwrap();

        let hints = infer_bash_repo_hints(
            &attempt(&workspace, "(cd project && printf x >> src/a.rs)"),
            &ctx(&repo),
        );

        assert!(hints.iter().any(|hint| {
            hint.confidence == BashHintConfidence::Strong
                && hint.target_paths == vec![normalize_path_for_matching(&repo.join("src/a.rs"))]
        }));
    }

    #[test]
    fn relative_redirection_from_parent_resolves_into_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let repo = workspace.join("project");
        std::fs::create_dir_all(repo.join("src")).unwrap();

        let hints = infer_bash_repo_hints(
            &attempt(&workspace, "printf x >> project/src/a.rs"),
            &ctx(&repo),
        );

        assert!(hints.iter().any(|hint| {
            hint.confidence == BashHintConfidence::Strong
                && hint.target_paths == vec![normalize_path_for_matching(&repo.join("src/a.rs"))]
        }));
    }

    #[test]
    fn git_c_produces_repo_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let repo = workspace.join("project");
        std::fs::create_dir_all(&repo).unwrap();

        let hints = infer_bash_repo_hints(
            &attempt(&workspace, "git -C project apply /tmp/change.patch"),
            &ctx(&repo),
        );

        assert!(hints.iter().any(|hint| {
            hint.confidence == BashHintConfidence::Strong && hint.target_paths.is_empty()
        }));
    }

    #[test]
    fn tool_cwd_flag_produces_repo_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let repo = workspace.join("project");
        std::fs::create_dir_all(&repo).unwrap();

        let hints = infer_bash_repo_hints(&attempt(&workspace, "make -C project"), &ctx(&repo));

        assert!(hints.iter().any(|hint| {
            hint.confidence == BashHintConfidence::Strong && hint.target_paths.is_empty()
        }));
    }

    #[test]
    fn parent_relative_path_outside_repo_is_not_file_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let repo = workspace.join("project");
        std::fs::create_dir_all(repo.join("src")).unwrap();

        let hints = infer_bash_repo_hints(
            &attempt(&workspace, "cd project && printf x >> ../outside.txt"),
            &ctx(&repo),
        );

        assert!(!hints.iter().any(|hint| {
            hint.target_paths
                .contains(&normalize_path_for_matching(&workspace.join("outside.txt")))
        }));
    }

    #[test]
    fn weak_unparseable_command_produces_no_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let repo = workspace.join("project");
        std::fs::create_dir_all(&repo).unwrap();

        let hints = infer_bash_repo_hints(
            &attempt(&workspace, "REPO=project; cd \"$REPO\" && ./gen.sh"),
            &ctx(&repo),
        );

        assert!(hints.is_empty());
    }
}
