use crate::config;

/// Extract recognized Git global flags from args so they can be placed
/// before the `log` subcommand. Everything else passes through to `git log`.
///
/// We deliberately skip the ambiguous short forms `-p` (paginate vs patch),
/// `-P` (no-pager vs perl-regexp), `-C` (change-dir vs copy-detection),
/// and bare `-c` (config vs combined-diff).
/// Their long-form equivalents (`--paginate`, `--no-pager`, `--git-dir`,
/// `--work-tree`, `-c key=val`) are handled correctly.
fn extract_git_global_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut global_args: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // `--` marks the end of options; everything after is a pathspec.
        if arg == "--" {
            rest.extend_from_slice(&args[i..]);
            break;
        }

        // --- Global no-value long options (unambiguous with git log) ---
        if matches!(
            arg.as_str(),
            "--paginate"
                | "--no-pager"
                | "--no-replace-objects"
                | "--no-lazy-fetch"
                | "--no-optional-locks"
                | "--no-advice"
                | "--bare"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
        ) {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // --- Global takes-value long options: --opt=val or --opt val ---
        if matches!(
            arg.as_str(),
            "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--config-env"
                | "--list-cmds"
                | "--attr-source"
                | "--super-prefix"
        ) {
            global_args.push(arg.clone());
            if i + 1 < args.len() {
                global_args.push(args[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        // --exec-path can be standalone (query) or --exec-path=<path> (set)
        if arg == "--exec-path" {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // =<value> forms for all long takes-value options
        if arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--namespace=")
            || arg.starts_with("--config-env=")
            || arg.starts_with("--list-cmds=")
            || arg.starts_with("--attr-source=")
            || arg.starts_with("--super-prefix=")
            || arg.starts_with("--exec-path=")
        {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // --- Short flags that are unambiguous ---

        // -C is deliberately NOT extracted:
        //   git global: -C <path> (change directory before doing anything)
        //   git log:    -C (detect copies, no argument)
        // Since all args arrive after the `log` keyword is stripped, a bare
        // `-C` is far more likely to be copy-detection. Users needing the
        // global form should use `--git-dir` or `--work-tree` instead.

        // -c <key>=<value>: git config override.
        // Git config keys are always `section.variable=value`, so a valid
        // assignment contains a '.' before the first '='.  A bare `-c`
        // without such a token is git log's combined-diff flag, and a next
        // token like `--format=%H` is a log option (no dot in key portion).
        if arg == "-c"
            && i + 1 < args.len()
            && args[i + 1]
                .find('=')
                .is_some_and(|eq| args[i + 1][..eq].contains('.'))
        {
            global_args.push(arg.clone());
            global_args.push(args[i + 1].clone());
            i += 2;
            continue;
        }

        // -c<key>=<value> sticky form — apply same dot-check as the spaced form
        if arg.starts_with("-c")
            && arg.len() > 2
            && arg[2..]
                .find('=')
                .is_some_and(|eq| arg[2..2 + eq].contains('.'))
        {
            global_args.push(arg.clone());
            i += 1;
            continue;
        }

        // -p, -P, and -C are deliberately NOT extracted:
        //   -p = git log --patch (not --paginate)
        //   -P = git log --perl-regexp (not --no-pager)
        //   -C = git log copy-detection (not --git-dir/change-dir)

        // Everything else (including --help, --version, -h, -v, -p, -P, -C,
        // and all git-log options) passes through to git log.
        rest.push(arg.clone());
        i += 1;
    }

    (global_args, rest)
}

/// Handle the `git ai log` command by proxying to `git log --notes=ai`.
///
/// All additional arguments are forwarded to `git log` as-is, allowing
/// users to filter, format, and paginate the output just like native git log.
///
/// Returns the `ExitStatus` from the child `git log` process so the caller
/// can handle telemetry and signal-aware exit.
pub fn handle_log(args: &[String]) -> std::process::ExitStatus {
    // Separate git-global flags from log arguments.
    let (global_args, log_args) = extract_git_global_args(args);

    // Build: git [global_args] log --notes=ai [log_args...]
    let git_cmd = config::Config::get().git_cmd().to_string();
    let mut cmd = std::process::Command::new(&git_cmd);
    cmd.args(&global_args);
    cmd.arg("log");
    cmd.arg("--notes=ai");
    cmd.args(&log_args);

    // Inherit stdin/stdout/stderr so the pager and terminal colors work
    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    match cmd.status() {
        Ok(status) => status,
        Err(e) => {
            eprintln!("Failed to execute git log: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // -- double-dash separator --

    #[test]
    fn double_dash_stops_extraction() {
        let (global, rest) = extract_git_global_args(&s(&["--", "--bare"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["--", "--bare"]));
    }

    #[test]
    fn double_dash_after_global_arg() {
        let (global, rest) = extract_git_global_args(&s(&["--paginate", "--", "--bare"]));
        assert_eq!(global, s(&["--paginate"]));
        assert_eq!(rest, s(&["--", "--bare"]));
    }

    #[test]
    fn double_dash_alone() {
        let (global, rest) = extract_git_global_args(&s(&["--"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["--"]));
    }

    #[test]
    fn double_dash_as_last_after_log_args() {
        let (global, rest) = extract_git_global_args(&s(&["--oneline", "--"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["--oneline", "--"]));
    }

    // -- no-value global flags --

    #[test]
    fn no_value_global_flags_extracted() {
        let (global, rest) = extract_git_global_args(&s(&["--paginate", "--oneline"]));
        assert_eq!(global, s(&["--paginate"]));
        assert_eq!(rest, s(&["--oneline"]));
    }

    #[test]
    fn bare_flag_extracted() {
        let (global, rest) = extract_git_global_args(&s(&["--bare", "--graph"]));
        assert_eq!(global, s(&["--bare"]));
        assert_eq!(rest, s(&["--graph"]));
    }

    // -- takes-value global options --

    #[test]
    fn git_dir_spaced_form() {
        let (global, rest) = extract_git_global_args(&s(&["--git-dir", "/some/path", "--oneline"]));
        assert_eq!(global, s(&["--git-dir", "/some/path"]));
        assert_eq!(rest, s(&["--oneline"]));
    }

    #[test]
    fn git_dir_equals_form() {
        let (global, rest) = extract_git_global_args(&s(&["--git-dir=/some/path", "--oneline"]));
        assert_eq!(global, s(&["--git-dir=/some/path"]));
        assert_eq!(rest, s(&["--oneline"]));
    }

    #[test]
    fn takes_value_option_at_end_without_value() {
        let (global, rest) = extract_git_global_args(&s(&["--git-dir"]));
        assert_eq!(global, s(&["--git-dir"]));
        assert!(rest.is_empty());
    }

    // -- exec-path --

    #[test]
    fn exec_path_standalone() {
        let (global, rest) = extract_git_global_args(&s(&["--exec-path", "--oneline"]));
        assert_eq!(global, s(&["--exec-path"]));
        assert_eq!(rest, s(&["--oneline"]));
    }

    #[test]
    fn exec_path_equals_form() {
        let (global, rest) = extract_git_global_args(&s(&["--exec-path=/usr/lib/git", "--graph"]));
        assert_eq!(global, s(&["--exec-path=/usr/lib/git"]));
        assert_eq!(rest, s(&["--graph"]));
    }

    // -- -c config override --

    #[test]
    fn dash_c_with_valid_config_key() {
        let (global, rest) = extract_git_global_args(&s(&["-c", "core.pager=cat", "--oneline"]));
        assert_eq!(global, s(&["-c", "core.pager=cat"]));
        assert_eq!(rest, s(&["--oneline"]));
    }

    #[test]
    fn dash_c_without_dot_is_not_extracted() {
        // bare -c followed by something without section.key=val is git log's combined-diff
        let (global, rest) = extract_git_global_args(&s(&["-c", "foo=bar"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["-c", "foo=bar"]));
    }

    #[test]
    fn dash_c_followed_by_log_option() {
        let (global, rest) = extract_git_global_args(&s(&["-c", "--format=%H"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["-c", "--format=%H"]));
    }

    #[test]
    fn sticky_c_with_valid_config_key() {
        let (global, rest) = extract_git_global_args(&s(&["-ccore.pager=cat"]));
        assert_eq!(global, s(&["-ccore.pager=cat"]));
        assert!(rest.is_empty());
    }

    #[test]
    fn sticky_c_without_dot_is_not_extracted() {
        // -cC=3 should NOT be extracted — no dot in key portion
        let (global, rest) = extract_git_global_args(&s(&["-cC=3"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["-cC=3"]));
    }

    // -- ambiguous short flags are NOT extracted --

    #[test]
    fn dash_capital_c_not_extracted() {
        let (global, rest) = extract_git_global_args(&s(&["-C", "--oneline"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["-C", "--oneline"]));
    }

    #[test]
    fn dash_p_not_extracted() {
        let (global, rest) = extract_git_global_args(&s(&["-p"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["-p"]));
    }

    #[test]
    fn dash_capital_p_not_extracted() {
        let (global, rest) = extract_git_global_args(&s(&["-P"]));
        assert!(global.is_empty());
        assert_eq!(rest, s(&["-P"]));
    }

    // -- empty args --

    #[test]
    fn empty_args() {
        let (global, rest) = extract_git_global_args(&s(&[]));
        assert!(global.is_empty());
        assert!(rest.is_empty());
    }

    // -- mixed scenarios --

    #[test]
    fn multiple_global_args_with_log_args() {
        let (global, rest) = extract_git_global_args(&s(&[
            "--paginate",
            "-c",
            "core.pager=less",
            "--oneline",
            "--graph",
        ]));
        assert_eq!(global, s(&["--paginate", "-c", "core.pager=less"]));
        assert_eq!(rest, s(&["--oneline", "--graph"]));
    }

    #[test]
    fn global_args_then_double_dash_then_pathspecs() {
        let (global, rest) = extract_git_global_args(&s(&[
            "--no-pager",
            "--git-dir=/repo",
            "--oneline",
            "--",
            "src/",
            "--bare",
        ]));
        assert_eq!(global, s(&["--no-pager", "--git-dir=/repo"]));
        assert_eq!(rest, s(&["--oneline", "--", "src/", "--bare"]));
    }
}
