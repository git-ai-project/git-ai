use crate::commands::core_hooks::{
    PASSTHROUGH_ONLY_HOOKS, PREVIOUS_HOOKS_PATH_FILE, managed_core_hooks_dir,
    run_core_hook_best_effort,
};
use crate::utils::{
    GIT_AI_GIT_CMD_ENV, GIT_AI_SKIP_CORE_HOOKS_ENV, GIT_AI_TRAMPOLINE_SKIP_CHAIN_ENV, debug_log,
};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

const STREAMED_STDIN_HOOKS: &[&str] = &["pre-push", "reference-transaction", "post-rewrite"];

pub fn handle_hook_trampoline_command(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: git-ai hook-trampoline <hook-name> [hook-args...]");
        std::process::exit(1);
    }

    if std::env::var(GIT_AI_SKIP_CORE_HOOKS_ENV).as_deref() == Ok("1") {
        return;
    }

    if std::env::var_os(GIT_AI_GIT_CMD_ENV).is_none() {
        // Hook paths are hot; keep git command resolution local and avoid loading full config.
        // SAFETY: set once per process for this hook subprocess.
        unsafe {
            std::env::set_var(GIT_AI_GIT_CMD_ENV, "git");
        }
    }

    let hook_name = args[0].as_str();
    let hook_args = &args[1..];

    let stdin_bytes = if uses_streamed_stdin(hook_name) {
        read_stdin_bytes()
    } else {
        Vec::new()
    };
    let stdin_string = if stdin_bytes.is_empty() || !hook_uses_stdin_string(hook_name) {
        None
    } else {
        Some(String::from_utf8_lossy(&stdin_bytes).to_string())
    };

    if should_dispatch_to_core_hook(hook_name, hook_args, stdin_string.as_deref()) {
        run_core_hook_best_effort(hook_name, hook_args, stdin_string.as_deref());
    }

    let skip_chain = std::env::var(GIT_AI_TRAMPOLINE_SKIP_CHAIN_ENV).as_deref() == Ok("1");
    if !skip_chain
        && let Some(status) = run_chained_hook(hook_name, hook_args, stdin_bytes.as_slice())
        && !status.success()
    {
        exit_with_status(status);
    }
}

fn uses_streamed_stdin(hook_name: &str) -> bool {
    STREAMED_STDIN_HOOKS.contains(&hook_name)
}

fn hook_uses_stdin_string(hook_name: &str) -> bool {
    matches!(hook_name, "reference-transaction" | "post-rewrite")
}

fn read_stdin_bytes() -> Vec<u8> {
    let mut buf = Vec::new();
    if let Err(error) = std::io::stdin().read_to_end(&mut buf) {
        debug_log(&format!("hook trampoline failed reading stdin: {}", error));
        return Vec::new();
    }
    buf
}

fn should_dispatch_to_core_hook(
    hook_name: &str,
    hook_args: &[String],
    stdin: Option<&str>,
) -> bool {
    if PASSTHROUGH_ONLY_HOOKS.contains(&hook_name) {
        return false;
    }

    if hook_name != "reference-transaction" {
        if hook_name == "post-index-change" {
            return has_pending_stash_apply_marker();
        }
        return true;
    }

    let stage = hook_args.first().map(String::as_str).unwrap_or_default();
    if stage != "prepared" && stage != "committed" {
        return false;
    }

    if classify_ref_transaction_action_from_env() == RefTxnActionClass::CommitLike {
        return false;
    }

    let Some(stdin) = stdin else {
        return false;
    };

    reference_transaction_has_relevant_refs(stdin)
}

fn reference_transaction_has_relevant_refs(stdin: &str) -> bool {
    for line in stdin.lines() {
        let mut parts = line.split_whitespace();
        let old = match parts.next() {
            Some(old) => old,
            None => continue,
        };
        let new = match parts.next() {
            Some(new) => new,
            None => continue,
        };
        let reference = match parts.next() {
            Some(reference) => reference,
            None => continue,
        };

        if old == new {
            continue;
        }

        if reference == "ORIG_HEAD"
            || reference == "refs/stash"
            || reference == "CHERRY_PICK_HEAD"
            || reference.starts_with("refs/remotes/")
            || (reference == "AUTO_MERGE" && is_zero_oid(old) && !is_zero_oid(new))
        {
            return true;
        }
    }

    false
}

fn is_zero_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.chars().all(|c| c == '0')
}

fn has_pending_stash_apply_marker() -> bool {
    let git_dir = std::env::var_os("GIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".git"));
    let git_dir = if git_dir.is_relative() {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(git_dir))
            .unwrap_or_else(|_| PathBuf::from(".git"))
    } else {
        git_dir
    };

    let state_path = git_dir.join("ai").join("core_hook_state.json");
    let Ok(content) = fs::read_to_string(state_path) else {
        return false;
    };
    content.contains("\"pending_stash_apply\":{")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefTxnActionClass {
    Unknown,
    CommitLike,
}

fn classify_ref_transaction_action_from_env() -> RefTxnActionClass {
    let Some(action) = std::env::var("GIT_REFLOG_ACTION")
        .ok()
        .map(|action| action.trim().to_string())
        .filter(|action| !action.is_empty())
    else {
        return RefTxnActionClass::Unknown;
    };

    if action.starts_with("commit") {
        return RefTxnActionClass::CommitLike;
    }

    RefTxnActionClass::Unknown
}

fn run_chained_hook(
    hook_name: &str,
    hook_args: &[String],
    stdin_bytes: &[u8],
) -> Option<ExitStatus> {
    if let Some(previous_hook) = previous_hook_path(hook_name) {
        return run_single_chained_hook(&previous_hook, hook_args, stdin_bytes);
    }

    let repo_hook = repository_hook_path(hook_name);
    run_single_chained_hook(&repo_hook, hook_args, stdin_bytes)
}

fn previous_hook_path(hook_name: &str) -> Option<PathBuf> {
    let managed_dir = managed_core_hooks_dir().ok()?;
    let previous_file = managed_dir.join(PREVIOUS_HOOKS_PATH_FILE);
    let previous_meta = fs::metadata(&previous_file).ok()?;
    if !previous_meta.is_file() || previous_meta.len() == 0 {
        return None;
    }

    let raw_previous = fs::read_to_string(previous_file).ok()?;
    let raw_previous = raw_previous.trim();
    if raw_previous.is_empty() {
        return None;
    }

    let previous_dir = expand_tilde_path(raw_previous);
    if same_path(&previous_dir, &managed_dir) {
        return None;
    }

    Some(previous_dir.join(hook_name))
}

fn repository_hook_path(hook_name: &str) -> PathBuf {
    let mut git_dir = std::env::var_os("GIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".git"));

    if git_dir.is_relative()
        && let Ok(current_dir) = std::env::current_dir()
    {
        git_dir = current_dir.join(git_dir);
    }

    git_dir.join("hooks").join(hook_name)
}

fn run_single_chained_hook(
    hook_path: &Path,
    hook_args: &[String],
    stdin_bytes: &[u8],
) -> Option<ExitStatus> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = fs::metadata(hook_path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        if is_managed_hook_path(hook_path) {
            return None;
        }

        let mut command = if metadata.permissions().mode() & 0o111 != 0 {
            Command::new(hook_path)
        } else {
            let mut command = Command::new("sh");
            command.arg(hook_path);
            command
        };
        command.args(hook_args);
        return run_command_with_stdin(command, stdin_bytes).ok();
    }

    #[cfg(windows)]
    {
        let metadata = fs::metadata(hook_path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        if is_managed_hook_path(hook_path) {
            return None;
        }

        let mut command = Command::new("sh");
        command.arg(hook_path);
        command.args(hook_args);
        return run_command_with_stdin(command, stdin_bytes).ok();
    }

    #[allow(unreachable_code)]
    None
}

fn run_command_with_stdin(mut command: Command, stdin_bytes: &[u8]) -> std::io::Result<ExitStatus> {
    if stdin_bytes.is_empty() {
        return command.status();
    }

    command.stdin(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_bytes)?;
    }
    child.wait()
}

fn is_managed_hook_path(hook_path: &Path) -> bool {
    let managed_dir = match managed_core_hooks_dir() {
        Ok(path) => path,
        Err(_) => return false,
    };

    let managed_hook_name = hook_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_default();
    if managed_hook_name.is_empty() {
        return false;
    }

    let managed_hook = managed_dir.join(managed_hook_name);
    same_path(hook_path, &managed_hook)
}

fn expand_tilde_path(raw: &str) -> PathBuf {
    if raw == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }

    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }

    PathBuf::from(raw)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(a_canonical), Ok(b_canonical)) = (a.canonicalize(), b.canonicalize()) {
        return normalize_path_for_compare(&a_canonical)
            == normalize_path_for_compare(&b_canonical);
    }

    normalize_path_for_compare(a) == normalize_path_for_compare(b)
}

fn normalize_path_for_compare(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    while normalized.ends_with('/') {
        if normalized == "/" {
            break;
        }
        #[cfg(windows)]
        if normalized.len() == 3
            && normalized.as_bytes()[1] == b':'
            && normalized.as_bytes()[2] == b'/'
        {
            break;
        }
        normalized.pop();
    }
    #[cfg(windows)]
    normalized.make_ascii_lowercase();
    normalized
}

fn exit_with_status(status: ExitStatus) -> ! {
    if let Some(code) = status.code() {
        std::process::exit(code);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            std::process::exit(128 + signal);
        }
    }

    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::{
        RefTxnActionClass, classify_ref_transaction_action_from_env,
        reference_transaction_has_relevant_refs, should_dispatch_to_core_hook,
    };
    use serial_test::serial;

    #[test]
    fn reference_transaction_prefilter_detects_relevant_refs() {
        assert!(reference_transaction_has_relevant_refs(
            "000 111 ORIG_HEAD\n"
        ));
        assert!(reference_transaction_has_relevant_refs(
            "000 111 refs/remotes/origin/main\n"
        ));
        assert!(!reference_transaction_has_relevant_refs(
            "000 111 refs/notes/commits\n"
        ));
    }

    #[test]
    fn should_dispatch_skips_passthrough_hooks() {
        assert!(!should_dispatch_to_core_hook("commit-msg", &[], None));
    }

    #[test]
    fn should_dispatch_reference_transaction_requires_stage_and_relevant_refs() {
        assert!(!should_dispatch_to_core_hook(
            "reference-transaction",
            &["aborted".to_string()],
            Some("000 111 ORIG_HEAD\n"),
        ));
        assert!(!should_dispatch_to_core_hook(
            "reference-transaction",
            &["prepared".to_string()],
            Some("000 111 refs/notes/commits\n"),
        ));
        assert!(should_dispatch_to_core_hook(
            "reference-transaction",
            &["prepared".to_string()],
            Some("000 111 ORIG_HEAD\n"),
        ));
    }

    #[test]
    fn should_dispatch_post_index_change_requires_pending_state() {
        assert!(!should_dispatch_to_core_hook(
            "post-index-change",
            &[],
            None
        ));
    }

    #[test]
    #[serial]
    fn reference_transaction_commit_like_action_is_skipped() {
        // SAFETY: this test is serialized to avoid concurrent env var mutation.
        unsafe {
            std::env::set_var("GIT_REFLOG_ACTION", "commit (amend): update");
        }
        assert_eq!(
            classify_ref_transaction_action_from_env(),
            RefTxnActionClass::CommitLike
        );
        assert!(!should_dispatch_to_core_hook(
            "reference-transaction",
            &["prepared".to_string()],
            Some("000 111 ORIG_HEAD\n"),
        ));
        // SAFETY: this test is serialized to avoid concurrent env var mutation.
        unsafe {
            std::env::remove_var("GIT_REFLOG_ACTION");
        }
    }
}
