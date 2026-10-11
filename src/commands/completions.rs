//! `git-ai completions <shell>` — print a static shell completion script.
//!
//! This subcommand is a *printing* convenience: it writes a completion script to
//! stdout for the caller to source however they choose. It never edits shell
//! config files, and it never shells out to git (that would violate the
//! constant-time / no-git-spawn rules for this codebase).
//!
//! ponytail: `COMMANDS` below is a hand-maintained mirror of the top-level
//! `match args[0].as_str()` dispatch in `git_ai_handlers.rs`. It will drift the
//! moment someone adds/renames a subcommand or long flag there. The real fix is
//! a parser-derived CLI (clap) + `clap_complete`; until then, keep this table in
//! sync manually.

const USAGE: &str = "usage: git-ai completions <bash|zsh|fish>";

/// One top-level subcommand and the long flags it accepts.
struct Command {
    name: &'static str,
    description: &'static str,
    flags: &'static [&'static str],
}

/// Single source of truth for the completion command surface.
///
/// Consumed by all three shell generators so the output cannot drift between
/// shells. See the module-level `ponytail:` note about keeping it in sync.
const COMMANDS: &[Command] = &[
    Command {
        name: "checkpoint",
        description: "Checkpoint working changes and attribute authorship",
        flags: &["--hook-input"],
    },
    Command {
        name: "log",
        description: "Show commit log with AI authorship stats",
        flags: &["--raw", "--notes"],
    },
    Command {
        name: "blame",
        description: "Git blame with AI authorship overlay",
        flags: &[],
    },
    Command {
        name: "diff",
        description: "Show diff with AI authorship annotations",
        flags: &["--json", "--include-stats", "--all-prompts"],
    },
    Command {
        name: "stats",
        description: "Show AI authorship statistics for a commit",
        flags: &["--json"],
    },
    Command {
        name: "usage",
        description: "Show local AI usage statistics",
        flags: &["--period", "--json"],
    },
    Command {
        name: "analyze",
        description: "Analyze agent sessions and effectiveness",
        flags: &[],
    },
    Command {
        name: "status",
        description: "Show uncommitted AI authorship status",
        flags: &["--json", "--diff-only"],
    },
    Command {
        name: "show",
        description: "Display authorship logs for a revision or range",
        flags: &[],
    },
    Command {
        name: "show-prompt",
        description: "Display a prompt record by its ID",
        flags: &["--commit", "--offset"],
    },
    Command {
        name: "config",
        description: "View and manage git-ai configuration",
        flags: &["--add"],
    },
    Command {
        name: "debug",
        description: "Print support/debug diagnostics",
        flags: &[],
    },
    Command {
        name: "bg",
        description: "Run and control the git-ai background service",
        flags: &[],
    },
    Command {
        name: "install",
        description: "Install hooks for AI authorship tracking",
        flags: &["--env", "--wsl", "--skills", "--visual-studio-extension"],
    },
    Command {
        name: "install-hooks",
        description: "Install hooks for AI authorship tracking",
        flags: &["--env", "--wsl", "--skills", "--visual-studio-extension"],
    },
    Command {
        name: "uninstall",
        description: "Remove git-ai hooks from all detected tools",
        flags: &[],
    },
    Command {
        name: "uninstall-hooks",
        description: "Remove git-ai hooks from all detected tools",
        flags: &[],
    },
    Command {
        name: "ci",
        description: "Continuous integration utilities",
        flags: &[],
    },
    Command {
        name: "git-path",
        description: "Print the path to the underlying git executable",
        flags: &[],
    },
    Command {
        name: "await",
        description: "Wait for the background service to finish all work",
        flags: &["--timeout"],
    },
    Command {
        name: "reingest",
        description: "Redeliver locally stored metric events",
        flags: &["--all", "--from", "--to", "--since"],
    },
    Command {
        name: "upgrade",
        description: "Check for updates and install if available",
        flags: &["--force"],
    },
    Command {
        name: "fetch-notes",
        description: "Synchronously fetch AI authorship notes",
        flags: &["--remote", "--json"],
    },
    Command {
        name: "dashboard",
        description: "Open the personal dashboard",
        flags: &[],
    },
    Command {
        name: "login",
        description: "Authenticate with Git AI",
        flags: &[],
    },
    Command {
        name: "logout",
        description: "Clear stored credentials",
        flags: &[],
    },
    Command {
        name: "whoami",
        description: "Show auth state and login identity",
        flags: &[],
    },
    Command {
        name: "exchange-nonce",
        description: "Exchange an authentication nonce",
        flags: &[],
    },
    Command {
        name: "version",
        description: "Print the git-ai version",
        flags: &["-v", "--version"],
    },
    Command {
        name: "help",
        description: "Show the git-ai help message",
        flags: &["-h", "--help"],
    },
];

/// Extra command names that resolve to a canonical [`COMMANDS`] entry.
const ALIASES: &[(&str, &str)] = &[("d", "bg"), ("daemon", "bg"), ("dash", "dashboard")];

/// The canonical command named `name`, if any.
fn find_command(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// Every top-level word (canonical names followed by aliases), in stable order.
fn command_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = COMMANDS.iter().map(|c| c.name).collect();
    names.extend(ALIASES.iter().map(|(alias, _)| *alias));
    names
}

/// Effective `(name, description, flags)` rows for generators.
///
/// Includes aliases so `git-ai d ...` completes the same flags as `git-ai bg`.
fn effective_commands() -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    let mut rows: Vec<(&'static str, &'static str, &'static [&'static str])> = COMMANDS
        .iter()
        .map(|c| (c.name, c.description, c.flags))
        .collect();
    for (alias, canonical) in ALIASES {
        if let Some(target) = find_command(canonical) {
            rows.push((alias, target.description, target.flags));
        }
    }
    rows
}

fn generate_bash() -> String {
    String::new()
}

fn generate_zsh() -> String {
    String::new()
}

fn generate_fish() -> String {
    String::new()
}

/// Build the completion script for `shell`, or `None` if the shell is unknown.
fn shell_script(shell: &str) -> Option<String> {
    match shell {
        "bash" => Some(generate_bash()),
        "zsh" => Some(generate_zsh()),
        "fish" => Some(generate_fish()),
        _ => None,
    }
}

pub fn handle_completions(args: &[String]) {
    let shell = match args.first() {
        Some(shell) => shell.as_str(),
        None => {
            eprintln!("{USAGE}");
            std::process::exit(1);
        }
    };

    match shell_script(shell) {
        Some(script) => print!("{script}"),
        None => {
            eprintln!("error: unsupported shell '{shell}'");
            eprintln!("{USAGE}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHELLS: [&str; 3] = ["bash", "zsh", "fish"];

    fn script(shell: &str) -> String {
        shell_script(shell).expect("known shell yields a script")
    }

    #[test]
    fn every_shell_emits_a_nonempty_script() {
        for shell in SHELLS {
            assert!(!script(shell).trim().is_empty(), "{shell} script was empty");
        }
    }

    #[test]
    fn every_command_and_alias_appears_in_every_shell() {
        let names = command_names();
        for shell in SHELLS {
            let out = script(shell);
            for name in &names {
                assert!(
                    out.contains(name),
                    "{shell} script missing command/alias '{name}'"
                );
            }
        }
    }

    #[test]
    fn bash_has_bash_specific_markers() {
        let out = script("bash");
        assert!(out.contains("complete -o default -F _git_ai_completions"), "{out}");
        assert!(out.contains("compgen -W"), "{out}");
        assert!(out.contains("COMPREPLY"), "{out}");
    }

    #[test]
    fn zsh_has_zsh_specific_markers() {
        let out = script("zsh");
        assert!(out.contains("#compdef git-ai"), "{out}");
        assert!(out.contains("compdef _git_ai git-ai"), "{out}");
        assert!(out.contains("_arguments"), "{out}");
    }

    #[test]
    fn fish_has_fish_specific_markers() {
        let out = script("fish");
        assert!(out.contains("complete -c git-ai"), "{out}");
        assert!(out.contains("__fish_use_subcommand"), "{out}");
        assert!(out.contains("__fish_seen_subcommand_from"), "{out}");
    }

    #[test]
    fn long_flags_are_attributed_to_their_command() {
        // A representative command whose flags must not leak to the top level.
        let bash = script("bash");
        let diff_case = bash
            .split("diff)")
            .nth(1)
            .expect("bash has a diff case arm");
        assert!(diff_case.contains("--include-stats"), "{bash}");
        assert!(diff_case.contains("--all-prompts"), "{bash}");

        let fish = script("fish");
        assert!(
            fish.contains("__fish_seen_subcommand_from diff") && fish.contains("-l include-stats"),
            "{fish}"
        );
    }

    #[test]
    fn missing_and_unknown_shells_are_rejected() {
        for shell in ["", "powershell", "BASH", "--help"] {
            assert!(
                shell_script(shell).is_none(),
                "unknown shell '{shell}' should not produce a script"
            );
        }
    }
}
