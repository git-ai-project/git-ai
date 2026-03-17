/// Parse the arguments that come *after* the `git` executable.
/// Example input corresponds to: `git -C .. commit -m foo`  => args = ["-C","..","commit","-m","foo"]
///
/// Rules:
/// - Only recognized Git *global* options are placed into `global_args`.
/// - The first non-option token (that isn't consumed as a value to a preceding global option)
///   is taken as the `command`.
/// - Everything after the command is `command_args`.
/// - If there is **no** command (e.g. `git --version`), then meta top-level options like
///   `--version`, `--help`, `--exec-path[=path]`, `--html-path`, `--man-path`, `--info-path`
///   are treated as `command_args` (never as `global_args`).
/// - Supports `--long=VAL`, `--long VAL`, `-Cpath`, `-C path`, `-cname=value`, and `-c name=value`.
///
/// This does *not* attempt to validate combinations or emulate Git's error paths.
/// It is intentionally permissive and order-preserving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedGitInvocation {
    pub global_args: Vec<String>,
    pub command: Option<String>,
    pub command_args: Vec<String>,
    /// Whether a top-level `--` was present between global args and the command.
    pub saw_end_of_opts: bool,
    /// True if this invocation requests help: presence of -h/--help or `help` command.
    pub is_help: bool,
}

impl ParsedGitInvocation {
    /// Return the argv *after* `git` as tokens, in order:
    ///   global_args [+ command] + command_args
    ///
    /// Note: this reconstructs *what we stored*. Re-inserts a top-level `--` if it was present.
    pub fn to_invocation_vec(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(
            self.global_args.len()
                + self.command_args.len()
                + usize::from(self.command.is_some())
                + usize::from(self.saw_end_of_opts),
        );
        v.extend(self.global_args.iter().cloned());
        if self.saw_end_of_opts {
            v.push("--".to_string());
        }
        if let Some(cmd) = &self.command {
            v.push(cmd.clone());
        }
        v.extend(self.command_args.iter().cloned());
        v
    }
    pub fn has_command_flag(&self, flag: &str) -> bool {
        self.command_args.iter().any(|arg| arg == flag)
    }

    /// Returns the n-th positional argument after the command (0-indexed).
    /// Skips all arguments that start with '-' (flags and their inline values).
    ///
    /// Examples:
    /// - `git merge abc --squash` => pos_command(0) returns Some("abc")
    /// - `git merge --squash --no-verify abc` => pos_command(0) returns Some("abc")
    /// - `git merge abc def --squash` => pos_command(1) returns Some("def")
    pub fn pos_command(&self, n: u8) -> Option<String> {
        let mut positional_count = 0u8;
        let mut skip_next = false;

        for arg in &self.command_args {
            // If we're skipping this arg because it's a value for a previous flag
            if skip_next {
                skip_next = false;
                continue;
            }

            // Skip flags
            if arg.starts_with('-') {
                // Check if this is a flag that takes a separate value
                // (e.g., -m, -X, --message without =)
                if arg.contains('=') {
                    // Flag with inline value like --message=foo, count as one arg
                    continue;
                } else if is_flag_with_value(arg) {
                    // Flag that takes the next arg as its value
                    skip_next = true;
                    continue;
                } else {
                    // Flag without value
                    continue;
                }
            }

            // This is a positional argument
            if positional_count == n {
                return Some(arg.clone());
            }
            positional_count += 1;
        }

        None
    }

    /// Returns all arguments after the `--` separator in command_args.
    /// These are typically pathspecs (file paths) that should be treated literally.
    ///
    /// Examples:
    /// - `git checkout -- file.txt` => pathspecs() returns vec!["file.txt"]
    /// - `git reset HEAD -- a.txt b.txt` => pathspecs() returns vec!["a.txt", "b.txt"]
    /// - `git checkout main` => pathspecs() returns vec![]
    pub fn pathspecs(&self) -> Vec<String> {
        if let Some(separator_pos) = self.command_args.iter().position(|arg| arg == "--") {
            self.command_args[separator_pos + 1..].to_vec()
        } else {
            Vec::new()
        }
    }
}

/// Returns true if the given flag typically takes a value as the next argument.
/// This is a heuristic for common git command flags that take values.
pub fn is_flag_with_value(flag: &str) -> bool {
    matches!(
        flag,
        // Commit/merge message flags
        "-m" | "--message" |
        "-F" | "--file" |
        "-t" | "--template" |
        "-e" | "--edit" |
        "--author" | "--date" |
        // Merge strategy
        "-s" | "--strategy" |
        "-X" | "--strategy-option" |
        // Log/diff flags
        "--since" | "--until" | "--before" | "--after" |
        "--format" | "--pretty" |
        "-n" | "--max-count" |
        "--skip" |
        // Checkout/branch flags
        "-b" | "-B" |
        // Push/pull flags
        "-u" | "--set-upstream" |
        // Config flags
        "--config" |
        // Misc
        "--depth" | "--shallow-since"
    )
}

pub fn parse_git_cli_args(args: &[String]) -> ParsedGitInvocation {
    use Kind::*;

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    enum Kind {
        GlobalNoValue,
        GlobalTakesValue, // e.g., --exec-path[=path]
        MetaNoValue,      // e.g., --version, --help, --html-path, --man-path, --info-path
        Unknown,          // something starting with '-' that isn't recognized at top-level
    }

    // Helpers to recognize/parse options.
    fn is_eq_form(tok: &str, long: &str) -> bool {
        tok.len() > long.len() + 1 && tok.starts_with(long) && tok.as_bytes()[long.len()] == b'='
    }

    fn classify(tok: &str) -> Kind {
        // Meta top-level (treated as command args when no command):
        // --version/-v, --help/-h, and the *-path* queries.
        match tok {
            "-v" | "--version" => return MetaNoValue,
            "-h" | "--help" => return MetaNoValue,
            "--html-path" | "--man-path" | "--info-path" => return MetaNoValue,
            _ => {}
        }
        if tok == "--exec-path" || is_eq_form(tok, "--exec-path") {
            return GlobalTakesValue;
        }

        // Global no-value options.
        match tok {
            "-p"
            | "--paginate"
            | "-P"
            | "--no-pager"
            | "--no-replace-objects"
            | "--no-lazy-fetch"
            | "--no-optional-locks"
            | "--no-advice"
            | "--bare"
            | "--literal-pathspecs"
            | "--glob-pathspecs"
            | "--noglob-pathspecs"
            | "--icase-pathspecs" => return GlobalNoValue,
            _ => {}
        }

        // Global takes-value options (support both `--opt=VAL` and `--opt VAL`).
        if tok == "-C" || tok.starts_with("-C") {
            return GlobalTakesValue;
        } // allow -Cpath
        if tok == "-c" || tok.starts_with("-c") {
            return GlobalTakesValue;
        } // allow -cname=value
        if tok == "--git-dir" || is_eq_form(tok, "--git-dir") {
            return GlobalTakesValue;
        }
        if tok == "--work-tree" || is_eq_form(tok, "--work-tree") {
            return GlobalTakesValue;
        }
        if tok == "--namespace" || is_eq_form(tok, "--namespace") {
            return GlobalTakesValue;
        }
        if tok == "--config-env" || is_eq_form(tok, "--config-env") {
            return GlobalTakesValue;
        }
        if tok == "--list-cmds" || is_eq_form(tok, "--list-cmds") {
            return GlobalTakesValue;
        }
        if tok == "--attr-source" || is_eq_form(tok, "--attr-source") {
            return GlobalTakesValue;
        }
        // Seen in some builds' SYNOPSIS; treat as value-taking if present.
        if tok == "--super-prefix" || is_eq_form(tok, "--super-prefix") {
            return GlobalTakesValue;
        }

        // A plain `--` (end-of-options) is handled in the main loop.
        if tok == "--" {
            return Unknown;
        }

        // Anything else starting with '-' is unknown to top-level git option parsing.
        if tok.starts_with('-') {
            return Unknown;
        }

        // Non-dash token => not an option (caller decides whether it's the command).
        Unknown
    }

    // Consume one token that *may* have an attached value (e.g. `--opt=VAL`, `-Cpath`, `-cname=val`).
    // Returns (tokens_to_push, tokens_consumed).
    fn take_valueish(all: &[String], i: usize, key: &str) -> (Vec<String>, usize) {
        let tok = &all[i];

        // Long form with '=' (e.g. --git-dir=/x, --exec-path=/x, --config-env=name=ENV).
        if let Some(eq) = tok.find('=')
            && eq > 0
            && tok.starts_with("--")
        {
            return (vec![tok.clone()], 1);
        }

        // Short sticky for -Cpath / -cname=value
        if key == "-C" && tok != "-C" && tok.starts_with("-C") {
            return (vec![tok.clone()], 1);
        }
        if key == "-c" && tok != "-c" && tok.starts_with("-c") {
            return (vec![tok.clone()], 1);
        }

        // Separate value in next token (if present).
        if i + 1 < all.len() {
            return (vec![tok.clone(), all[i + 1].clone()], 2);
        }
        // No following value; just return the option and let downstream handle the error later.
        (vec![tok.clone()], 1)
    }

    let mut global_args = Vec::new();
    let mut command: Option<String> = None;
    let mut command_args = Vec::new();

    // If we see meta options *before* any command, we buffer them here.
    // If we end up with no command, we move them into command_args; otherwise we leave them out.
    // (Per your rule, e.g. `git --version` => command=None, command_args=["--version"]).
    let mut pre_command_meta: Vec<String> = Vec::new();

    // First pass: scan leading global options. Stop when we hit:
    // - `--` (then next token is *the command*, even if it starts with '-')
    // - a non-option token (that's the command)
    // - an unknown dash-option (treat as "no command", remaining go to command_args)
    let mut i = 0usize;
    let mut saw_end_of_opts = false;

    while i < args.len() {
        let tok = &args[i];

        if tok == "--" {
            saw_end_of_opts = true;
            i += 1;
            break;
        }

        match classify(tok) {
            GlobalNoValue => {
                global_args.push(tok.clone());
                i += 1;
            }
            GlobalTakesValue => {
                // Figure out which key we're handling to parse sticky forms.
                let key = if tok.starts_with("-C") {
                    "-C"
                } else if tok.starts_with("-c") {
                    "-c"
                } else if tok.starts_with("--git-dir") {
                    "--git-dir"
                } else if tok.starts_with("--work-tree") {
                    "--work-tree"
                } else if tok.starts_with("--namespace") {
                    "--namespace"
                } else if tok.starts_with("--config-env") {
                    "--config-env"
                } else if tok.starts_with("--list-cmds") {
                    "--list-cmds"
                } else if tok.starts_with("--attr-source") {
                    "--attr-source"
                } else if tok.starts_with("--super-prefix") {
                    "--super-prefix"
                } else {
                    ""
                };

                let (taken, consumed) = take_valueish(args, i, key);
                global_args.extend(taken);
                i += consumed;
            }
            MetaNoValue => {
                // Buffer meta; they'll become command_args iff no subcommand appears.
                pre_command_meta.push(tok.clone());
                i += 1;
            }
            Unknown => {
                if tok.starts_with('-') {
                    // Unknown top-level dash-option: treat as a meta-ish/invalid sequence.
                    // We won't assign a command; remaining tokens will become command_args later.
                    // Do not mutate `pre_command_meta` here; post-parse rewrites rely on it.
                    command = None;
                    break;
                } else {
                    // Non-dash token => this is the command.
                    break;
                }
            }
        }
    }

    // If we haven't decided the command yet:
    if command.is_none() {
        if i < args.len() {
            if saw_end_of_opts {
                // `--` forces the very next token to be "the command", even if it begins with '-'.
                command = Some(args[i].clone());
                i += 1;
            } else if !args[i].starts_with('-') {
                // Normal case: first non-dash token after globals is the command.
                command = Some(args[i].clone());
                i += 1;
            } else {
                // Only meta/unknown options; no command.
                command = None;
            }
        } else {
            command = None;
        }
    }

    // The remainder are command args (if we found a command).
    if command.is_some() {
        command_args.extend_from_slice(&args[i..]);
        // NOTE: we intentionally DO NOT inject pre_command_meta when a subcommand exists.
        // Example: `git --help commit` is internally converted to `git help commit`, but per
        // the project's requirement we treat meta as *not* global and don't try to rewrite.
        // If you want to emulate conversion, you can special-case it here.
    } else {
        // No command: meta options are considered "command args".
        command_args.extend(pre_command_meta.clone());
        command_args.extend_from_slice(&args[i..]);
    }

    // --- NEW: post-parse rewrite for help/version to match git(1) semantics ---
    // Top-level presence of -h/--help or -v/--version (before any command)
    let pre_has_help = pre_command_meta.iter().any(|t| t == "--help" || t == "-h");
    let pre_has_version = pre_command_meta
        .iter()
        .any(|t| t == "--version" || t == "-v");

    // NOTE: git docs: --help takes precedence over --version. (git(1) OPTIONS)
    // So we always check/perform help rewrites before version rewrites.
    if command.is_some() {
        // Case: `git --help <cmd> [rest]`  ==>  `git help <cmd> [rest]`
        if pre_has_help {
            let orig_cmd = command.take().unwrap();
            let mut new_args = vec![orig_cmd];
            // Pass trailing tokens after the command to `git help` unchanged.
            new_args.append(&mut command_args);
            command = Some("help".into());
            command_args = new_args;
        }
        // NEW: `git --version ...` should rewrite to `git version` even if we
        // happened to parse a command token. Help still takes precedence.
        else if pre_has_version {
            // Drop the previously parsed command entirely and keep only version-relevant flags.
            command = Some("version".into());

            // Build args for `git version`: keep pre-command meta except the first -v/--version.
            let mut new_args = Vec::new();
            let mut dropped_one_version = false;
            for t in pre_command_meta.iter() {
                if !dropped_one_version && (t == "--version" || t == "-v") {
                    dropped_one_version = true;
                    continue;
                }
                new_args.push(t.clone()); // e.g., "--build-options"
            }

            // Do NOT carry over the previously parsed command or its args.
            command_args = new_args;
        }
    } else {
        // No subcommand parsed.

        // Case: `git --help [<cmd>|<help-opts>]`  ==>  `git help [<cmd>|<help-opts>]`
        if pre_has_help {
            command = Some("help".into());

            // Build args for `git help`: keep pre-command meta except the first help token.
            let mut new_args: Vec<String> = Vec::new();
            let mut dropped_one_help = false;
            for t in pre_command_meta.iter() {
                if !dropped_one_help && (t == "--help" || t == "-h") {
                    dropped_one_help = true;
                    continue;
                }
                // Help takes precedence: drop any version tokens when rewriting to help
                if t == "--version" || t == "-v" {
                    continue;
                }
                new_args.push(t.clone());
            }
            // Plus anything we already copied into `command_args` (drop stray help/version tokens)
            for t in command_args.iter() {
                if t == "--help" || t == "-h" || t == "--version" || t == "-v" {
                    continue;
                }
                new_args.push(t.clone());
            }
            command_args = new_args;
        }
        // Case: `git --version [--build-options]`  ==>  `git version [--build-options]`
        // (Only rewrite version when no command; --help would have taken precedence above.)
        else if pre_has_version {
            command = Some("version".into());
            // Remove the first occurrence of -v/--version; drop any non-dash tokens (e.g., stray commands)
            let mut new_args = Vec::new();
            let mut dropped_one_version = false;
            for t in command_args.iter() {
                if !dropped_one_version && (t == "--version" || t == "-v") {
                    dropped_one_version = true;
                    continue;
                }
                if t.starts_with('-') {
                    new_args.push(t.clone());
                }
            }
            command_args = new_args;
        }
    }
    // --- End NEW block ---

    // Determine whether this invocation represents a help request.
    let is_help = command.as_deref() == Some("help")
        || command.as_deref() == Some("--help")
        || pre_command_meta.iter().any(|t| t == "--help" || t == "-h")
        || command_args.iter().any(|t| t == "--help" || t == "-h");

    ParsedGitInvocation {
        global_args,
        command,
        command_args,
        saw_end_of_opts,
        is_help,
    }
}

pub fn is_dry_run(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--dry-run")
}

/// Extract the target directory from git clone command arguments.
/// Returns the directory where the repository was cloned to.
///
/// Logic:
/// - First non-option positional arg is the repository URL
/// - Second non-option positional arg (if present) is the target directory
/// - If no directory specified, derive from last component of URL (strip .git suffix)
pub fn extract_clone_target_directory(args: &[String]) -> Option<String> {
    let mut positional_args = Vec::new();
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if !after_double_dash {
            // Check for -- separator
            if arg == "--" {
                after_double_dash = true;
                i += 1;
                continue;
            }

            // Skip options that take a value
            if is_flag_with_value(arg) {
                i += 2; // Skip both option and its value
                continue;
            }

            // Skip standalone options
            if arg.starts_with('-') {
                i += 1;
                continue;
            }
        }

        // This is a positional argument
        positional_args.push(arg.clone());
        i += 1;
    }

    // Need at least one positional arg (the repository URL)
    if positional_args.is_empty() {
        return None;
    }

    // If we have 2+ positional args, the second one is the target directory
    if positional_args.len() >= 2 {
        return Some(positional_args[1].clone());
    }

    // Otherwise, derive directory name from repository URL
    let repo_url = &positional_args[0];
    derive_directory_from_url(repo_url)
}

/// Derive the target directory name from a repository URL.
/// Mimics git's behavior of using the last path component, stripping .git suffix.
fn derive_directory_from_url(url: &str) -> Option<String> {
    // Remove trailing slashes
    let url = url.trim_end_matches('/');

    // Extract the last path component
    let last_component = if let Some(pos) = url.rfind('/') {
        &url[pos + 1..]
    } else if let Some(pos) = url.rfind(':') {
        // Handle SCP-like syntax: user@host:path
        &url[pos + 1..]
    } else {
        url
    };

    if last_component.is_empty() {
        return None;
    }

    // Strip .git suffix if present
    let dir_name = last_component
        .strip_suffix(".git")
        .unwrap_or(last_component);

    if dir_name.is_empty() {
        None
    } else {
        Some(dir_name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pos_command_basic() {
        // Test: git merge abc --squash
        let args = vec![
            "merge".to_string(),
            "abc".to_string(),
            "--squash".to_string(),
        ];
        let parsed = parse_git_cli_args(&args);
        assert_eq!(parsed.pos_command(0), Some("abc".to_string()));
        assert_eq!(parsed.pos_command(1), None);
    }

    #[test]
    fn test_pos_command_flags_before() {
        // Test: git merge --squash --no-verify abc
        let args = vec![
            "merge".to_string(),
            "--squash".to_string(),
            "--no-verify".to_string(),
            "abc".to_string(),
        ];
        let parsed = parse_git_cli_args(&args);
        assert_eq!(parsed.pos_command(0), Some("abc".to_string()));
        assert_eq!(parsed.pos_command(1), None);
    }

    #[test]
    fn test_pos_command_multiple_positional() {
        // Test: git merge abc def --squash
        let args = vec![
            "merge".to_string(),
            "abc".to_string(),
            "def".to_string(),
            "--squash".to_string(),
        ];
        let parsed = parse_git_cli_args(&args);
        assert_eq!(parsed.pos_command(0), Some("abc".to_string()));
        assert_eq!(parsed.pos_command(1), Some("def".to_string()));
        assert_eq!(parsed.pos_command(2), None);
    }

    #[test]
    fn test_pos_command_with_flag_value() {
        // Test: git commit -m "message" file.txt
        let args = vec![
            "commit".to_string(),
            "-m".to_string(),
            "message".to_string(),
            "file.txt".to_string(),
        ];
        let parsed = parse_git_cli_args(&args);
        assert_eq!(parsed.pos_command(0), Some("file.txt".to_string()));
        assert_eq!(parsed.pos_command(1), None);
    }

    #[test]
    fn test_pos_command_inline_flag_value() {
        // Test: git merge --strategy=recursive abc
        let args = vec![
            "merge".to_string(),
            "--strategy=recursive".to_string(),
            "abc".to_string(),
        ];
        let parsed = parse_git_cli_args(&args);
        assert_eq!(parsed.pos_command(0), Some("abc".to_string()));
    }

    #[test]
    fn test_derive_directory_from_url() {
        assert_eq!(
            derive_directory_from_url("https://github.com/user/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            derive_directory_from_url("https://github.com/user/repo"),
            Some("repo".to_string())
        );
        assert_eq!(
            derive_directory_from_url("git@github.com:user/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            derive_directory_from_url("user@host:path/to/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            derive_directory_from_url("/local/path/repo.git"),
            Some("repo".to_string())
        );
    }

    #[test]
    fn test_extract_clone_target_directory() {
        // Explicit directory specified
        let args = vec![
            "https://github.com/user/repo.git".to_string(),
            "my-dir".to_string(),
        ];
        assert_eq!(
            extract_clone_target_directory(&args),
            Some("my-dir".to_string())
        );

        // Directory derived from URL
        let args = vec!["https://github.com/user/repo.git".to_string()];
        assert_eq!(
            extract_clone_target_directory(&args),
            Some("repo".to_string())
        );

        // With options
        let args = vec![
            "-b".to_string(),
            "main".to_string(),
            "https://github.com/user/repo.git".to_string(),
        ];
        assert_eq!(
            extract_clone_target_directory(&args),
            Some("repo".to_string())
        );

        // With options and explicit directory
        let args = vec![
            "-b".to_string(),
            "main".to_string(),
            "https://github.com/user/repo.git".to_string(),
            "my-dir".to_string(),
        ];
        assert_eq!(
            extract_clone_target_directory(&args),
            Some("my-dir".to_string())
        );

        // With --option=value syntax
        let args = vec![
            "--branch=main".to_string(),
            "https://github.com/user/repo.git".to_string(),
            "my-dir".to_string(),
        ];
        assert_eq!(
            extract_clone_target_directory(&args),
            Some("my-dir".to_string())
        );
    }
}
