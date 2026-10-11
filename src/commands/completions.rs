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
    /// Advertised subcommands offered after the command name. Empty for most.
    subcommands: &'static [&'static str],
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
        subcommands: &[],
    },
    Command {
        name: "log",
        description: "Show commit log with AI authorship stats",
        flags: &["--raw", "--notes"],
        subcommands: &[],
    },
    Command {
        name: "blame",
        description: "Git blame with AI authorship overlay",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "diff",
        description: "Show diff with AI authorship annotations",
        flags: &["--json", "--include-stats", "--all-prompts"],
        subcommands: &[],
    },
    Command {
        name: "stats",
        description: "Show AI authorship statistics for a commit",
        flags: &["--json"],
        subcommands: &[],
    },
    Command {
        name: "usage",
        description: "Show local AI usage statistics",
        flags: &["--period", "--json"],
        subcommands: &[],
    },
    Command {
        name: "analyze",
        description: "Analyze agent sessions and effectiveness",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "status",
        description: "Show uncommitted AI authorship status",
        flags: &["--json", "--diff-only"],
        subcommands: &[],
    },
    Command {
        name: "show",
        description: "Display authorship logs for a revision or range",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "show-prompt",
        description: "Display a prompt record by its ID",
        flags: &["--commit", "--offset"],
        subcommands: &[],
    },
    Command {
        name: "config",
        description: "View and manage git-ai configuration",
        flags: &["--add"],
        subcommands: &[],
    },
    Command {
        name: "debug",
        description: "Print support/debug diagnostics",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "bg",
        description: "Run and control the git-ai background service",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "install",
        description: "Install hooks for AI authorship tracking",
        flags: &["--env", "--wsl", "--skills", "--visual-studio-extension"],
        subcommands: &[],
    },
    Command {
        name: "install-hooks",
        description: "Install hooks for AI authorship tracking",
        flags: &["--env", "--wsl", "--skills", "--visual-studio-extension"],
        subcommands: &[],
    },
    Command {
        name: "uninstall",
        description: "Remove git-ai hooks from all detected tools",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "uninstall-hooks",
        description: "Remove git-ai hooks from all detected tools",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "ci",
        description: "Continuous integration utilities",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "git-path",
        description: "Print the path to the underlying git executable",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "await",
        description: "Wait for the background service to finish all work",
        flags: &["--timeout"],
        subcommands: &[],
    },
    Command {
        name: "reingest",
        description: "Redeliver locally stored metric events",
        flags: &["--all", "--from", "--to", "--since"],
        subcommands: &[],
    },
    Command {
        name: "upgrade",
        description: "Check for updates and install if available",
        flags: &["--force"],
        subcommands: &[],
    },
    Command {
        name: "fetch-notes",
        description: "Synchronously fetch AI authorship notes",
        flags: &["--remote", "--json"],
        subcommands: &[],
    },
    Command {
        name: "notes",
        description: "Manage the notes backend",
        flags: &[],
        subcommands: &["migrate"],
    },
    Command {
        name: "dashboard",
        description: "Open the personal dashboard",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "login",
        description: "Authenticate with Git AI",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "logout",
        description: "Clear stored credentials",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "whoami",
        description: "Show auth state and login identity",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "exchange-nonce",
        description: "Exchange an authentication nonce",
        flags: &[],
        subcommands: &[],
    },
    Command {
        name: "version",
        description: "Print the git-ai version",
        flags: &["-v", "--version"],
        subcommands: &[],
    },
    Command {
        name: "help",
        description: "Show the git-ai help message",
        flags: &["-h", "--help"],
        subcommands: &[],
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
fn effective_commands() -> Vec<(
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
)> {
    let mut rows: Vec<_> = COMMANDS
        .iter()
        .map(|c| (c.name, c.description, c.flags, c.subcommands))
        .collect();
    for (alias, canonical) in ALIASES {
        if let Some(target) = find_command(canonical) {
            rows.push((alias, target.description, target.flags, target.subcommands));
        }
    }
    rows
}

/// Space-separated list of every top-level word (canonical names + aliases).
fn top_level_words() -> String {
    command_names().join(" ")
}

fn generate_bash() -> String {
    let mut out = String::new();
    out.push_str("# git-ai bash completion script (static).\n");
    out.push_str("# Generated by: git-ai completions bash\n");
    out.push_str(
        "# ponytail: hand-maintained command list; update it when the git-ai dispatch table changes.\n",
    );
    out.push('\n');
    out.push_str("_git_ai_completions() {\n");
    out.push_str("    local cur cmd\n");
    out.push_str("    COMPREPLY=()\n");
    out.push_str("    cur=\"${COMP_WORDS[COMP_CWORD]}\"\n");
    out.push('\n');
    out.push_str("    if [[ ${COMP_CWORD} -eq 1 ]]; then\n");
    out.push_str(&format!(
        "        COMPREPLY=($(compgen -W \"{}\" -- \"${{cur}}\"))\n",
        top_level_words()
    ));
    out.push_str("        return 0\n");
    out.push_str("    fi\n");
    out.push('\n');
    out.push_str("    cmd=\"${COMP_WORDS[1]}\"\n");
    out.push_str("    case \"${cmd}\" in\n");
    for (name, _description, flags, subcommands) in effective_commands() {
        // Subcommands (e.g. `notes migrate`) take precedence for the first
        // positional argument; commands that only have flags complete flags.
        let words = if subcommands.is_empty() {
            flags
        } else {
            subcommands
        };
        if words.is_empty() {
            continue;
        }
        out.push_str(&format!("        {name})\n"));
        out.push_str(&format!(
            "            COMPREPLY=($(compgen -W \"{}\" -- \"${{cur}}\"))\n",
            words.join(" ")
        ));
        out.push_str("            ;;\n");
    }
    out.push_str("    esac\n");
    out.push_str("    return 0\n");
    out.push_str("}\n");
    out.push('\n');
    out.push_str("complete -o default -F _git_ai_completions git-ai\n");
    out
}

fn generate_zsh() -> String {
    let mut out = String::new();
    out.push_str("#compdef git-ai\n");
    out.push_str("# git-ai zsh completion script (static).\n");
    out.push_str("# Generated by: git-ai completions zsh\n");
    out.push_str(
        "# ponytail: hand-maintained command list; update it when the git-ai dispatch table changes.\n",
    );
    out.push('\n');
    out.push_str("_git_ai() {\n");
    out.push_str("    local -a commands\n");
    out.push_str("    commands=(\n");
    for (name, description, _flags, _subcommands) in effective_commands() {
        out.push_str(&format!("        '{name}:{description}'\n"));
    }
    out.push_str("    )\n");
    out.push('\n');
    out.push_str("    if (( CURRENT == 2 )); then\n");
    out.push_str("        _describe -t commands 'git-ai command' commands\n");
    out.push_str("        return\n");
    out.push_str("    fi\n");
    out.push('\n');
    out.push_str("    case \"${words[2]}\" in\n");
    for (name, _description, flags, subcommands) in effective_commands() {
        if !subcommands.is_empty() {
            // Offer the command's subcommands (e.g. `notes migrate`).
            let var = format!("{}_subcommands", name.replace('-', "_"));
            out.push_str(&format!("        {name})\n"));
            out.push_str(&format!("            local -a {var}\n"));
            let quoted: Vec<String> = subcommands.iter().map(|sub| format!("'{sub}'")).collect();
            out.push_str(&format!("            {var}=({})\n", quoted.join(" ")));
            out.push_str(&format!(
                "            _describe -t commands 'git-ai {name} subcommand' {var}\n"
            ));
            out.push_str("            ;;\n");
            continue;
        }
        if flags.is_empty() {
            continue;
        }
        out.push_str(&format!("        {name})\n"));
        let quoted: Vec<String> = flags.iter().map(|flag| format!("'{flag}'")).collect();
        out.push_str(&format!("            _arguments {}\n", quoted.join(" ")));
        out.push_str("            ;;\n");
    }
    out.push_str("    esac\n");
    out.push_str("}\n");
    out.push('\n');
    out.push_str("compdef _git_ai git-ai\n");
    out
}

fn generate_fish() -> String {
    let mut out = String::new();
    out.push_str("# git-ai fish completion script (static).\n");
    out.push_str("# Generated by: git-ai completions fish\n");
    out.push_str(
        "# ponytail: hand-maintained command list; update it when the git-ai dispatch table changes.\n",
    );
    out.push('\n');
    out.push_str("complete -c git-ai -f\n");
    out.push('\n');
    out.push_str("# Subcommands\n");
    for (name, description, _flags, _subcommands) in effective_commands() {
        out.push_str(&format!(
            "complete -c git-ai -n '__fish_use_subcommand' -a {name} -d '{description}'\n"
        ));
    }
    out.push_str("\n# Nested subcommands\n");
    for (name, _description, _flags, subcommands) in effective_commands() {
        for sub in subcommands {
            out.push_str(&format!(
                "complete -c git-ai -n '__fish_seen_subcommand_from {name}' -a {sub}\n"
            ));
        }
    }
    out.push_str("\n# Flags\n");
    for (name, _description, flags, _subcommands) in effective_commands() {
        for flag in flags {
            let option = if let Some(long) = flag.strip_prefix("--") {
                format!("-l {long}")
            } else if let Some(short) = flag.strip_prefix('-') {
                format!("-s {short}")
            } else {
                continue;
            };
            out.push_str(&format!(
                "complete -c git-ai -n '__fish_seen_subcommand_from {name}' {option}\n"
            ));
        }
    }
    out
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
    fn notes_is_a_top_level_command() {
        let notes = find_command("notes").expect("`notes` must be in the command table");
        assert_eq!(
            notes.subcommands,
            &["migrate"],
            "`notes` advertises exactly the `migrate` subcommand"
        );
        assert!(
            command_names().contains(&"notes"),
            "`notes` must appear in the top-level word list"
        );
    }

    #[test]
    fn notes_and_migrate_appear_in_every_shell() {
        for shell in SHELLS {
            let out = script(shell);
            assert!(out.contains("notes"), "{shell} script missing 'notes'");
            assert!(
                out.contains("migrate"),
                "{shell} script missing 'migrate' subcommand"
            );
        }
    }

    #[test]
    fn notes_subcommand_is_offered_per_shell() {
        let bash = script("bash");
        // Anchor on the arm indentation so we don't match inside `fetch-notes)`.
        let notes_arm = bash
            .split("\n        notes)")
            .nth(1)
            .expect("bash has a notes case arm");
        assert!(notes_arm.contains("migrate"), "{bash}");

        let zsh = script("zsh");
        assert!(
            zsh.contains("notes)") && zsh.contains("'migrate"),
            "zsh must offer `migrate` in the notes subcommand context:\n{zsh}"
        );

        let fish = script("fish");
        assert!(
            fish.contains("__fish_seen_subcommand_from notes") && fish.contains("-a migrate"),
            "fish must offer `migrate` under notes:\n{fish}"
        );
    }

    #[test]
    fn hidden_internal_commands_are_not_advertised() {
        // `notes serve` is likewise hidden; the exact-subcommands assertion in
        // `notes_is_a_top_level_command` pins that, since "serve" is a substring
        // of the public word "service".
        const HIDDEN: [&str; 6] = [
            "git-hooks",
            "flush-metrics-db",
            "effective-ignore-patterns",
            "blame-analysis",
            "fetch-authorship-notes",
            "push-authorship-notes",
        ];
        for shell in SHELLS {
            let out = script(shell);
            for hidden in HIDDEN {
                assert!(
                    !out.contains(hidden),
                    "{shell} script must not advertise hidden command '{hidden}'"
                );
            }
        }
    }

    #[test]
    fn bash_has_bash_specific_markers() {
        let out = script("bash");
        assert!(
            out.contains("complete -o default -F _git_ai_completions"),
            "{out}"
        );
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
