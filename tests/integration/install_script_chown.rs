//! Guard test: keep `install.sh`'s post-install `chown` list in sync with the
//! agent hook installers.
//!
//! When git-ai is installed via an MDM (JAMF, JumpCloud, ...) the install
//! script runs as root, so `git-ai install-hooks` creates agent hook/config
//! directories owned by root. `install.sh` then `chown`s those directories
//! back to the real user; otherwise the user gets "permission denied" the next
//! time an agent (or a manual `git-ai install-hooks`) tries to write its hooks.
//!
//! The list of directories to `chown` lives in `install.sh`, but the source of
//! truth for which directories an agent actually writes to lives in
//! `src/mdm/agents/*.rs`. This test parses both and fails if an agent
//! references a home-relative dotfile directory that `install.sh` does not
//! cover, so the list cannot silently drift as new agents are added.
//!
//! Scope: home-relative dotfile dirs (`.foo` and `.config/foo`) referenced via
//! `home_dir().join(...)` in `src/mdm/agents/*.rs`. IDE settings paths under
//! `Library/Application Support/...` and the JetBrains directories are
//! intentionally out of scope — they are not agent dotfile dirs and are
//! handled separately/explicitly in `install.sh`.

use regex::Regex;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is the crate root (the git-ai repo root).
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Given the ordered path segments from a `home_dir().join(...)` chain, return
/// the directory we expect `install.sh` to `chown` — or `None` if this isn't a
/// home-relative dotfile directory we care about.
fn protected_dir(segments: &[String]) -> Option<String> {
    let first = segments.first()?;
    if !first.starts_with('.') {
        // e.g. "Library", "Applications" — not a dotfile hook dir.
        return None;
    }
    if first == ".config" {
        // ~/.config is shared across many tools; protect the per-app subdir.
        let second = segments.get(1)?;
        return Some(format!("{first}/{second}"));
    }
    Some(first.clone())
}

/// Collect every `.rs` file we want to scan for `home_dir().join(...)` chains.
///
/// Agents either reference their dotfile dir inline (`src/mdm/agents/*.rs`) or
/// via a small config-dir helper in `src/mdm/utils.rs` (e.g.
/// `claude_config_dir()` -> `home_dir().join(".claude")`), so we scan both.
fn source_files_to_scan() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let agents_dir = repo_root().join("src/mdm/agents");
    for entry in fs::read_dir(&agents_dir).expect("read src/mdm/agents") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    files.push(repo_root().join("src/mdm/utils.rs"));
    files
}

/// Extract the set of protected dotfile directories referenced by the agent
/// hook installers via `home_dir().join(...)` chains.
fn agent_dirs_from_source() -> BTreeSet<String> {
    // Match `home_dir()` followed by one or more `.join("literal")` calls,
    // allowing whitespace/newlines between calls (chains are often multi-line).
    // A trailing `.join(variable)` simply terminates the literal run, which is
    // handled by `protected_dir` returning None for incomplete `.config/*`.
    let chain_re = Regex::new(r#"home_dir\(\)\s*((?:\.join\("[^"]+"\)\s*)+)"#).unwrap();
    let literal_re = Regex::new(r#"\.join\("([^"]+)"\)"#).unwrap();

    let mut dirs = BTreeSet::new();
    for path in source_files_to_scan() {
        let source = fs::read_to_string(&path).unwrap();
        for chain in chain_re.captures_iter(&source) {
            let segments: Vec<String> = literal_re
                .captures_iter(&chain[1])
                .map(|c| c[1].to_string())
                .collect();
            if let Some(dir) = protected_dir(&segments) {
                dirs.insert(dir);
            }
        }
    }
    assert!(
        !dirs.is_empty(),
        "Found no home_dir().join(...) dotfile dirs in src/mdm — did the parsing break?",
    );
    dirs
}

/// Extract the set of `$HOME/...` directories that `install.sh` chowns.
fn install_sh_dirs() -> BTreeSet<String> {
    let install_sh = repo_root().join("install.sh");
    let source = fs::read_to_string(&install_sh).expect("read install.sh");
    // Lines look like:  "$HOME/.config/opencode" \
    let re = Regex::new(r#""\$HOME/([^"]+)""#).unwrap();
    re.captures_iter(&source)
        .map(|c| c[1].to_string())
        .collect()
}

#[test]
fn install_sh_lists_all_agent_dirs() {
    let agent_dirs = agent_dirs_from_source();
    let listed = install_sh_dirs();

    let missing: Vec<&String> = agent_dirs
        .iter()
        .filter(|dir| !listed.contains(*dir))
        .collect();

    assert!(
        missing.is_empty(),
        "install.sh is missing chown coverage for agent dotfile directories: {missing:?}\n\
         Add `\"$HOME/<dir>\" \\` to the chown loop in install.sh.\n\
         (Source: home_dir().join(...) chains in src/mdm/agents/*.rs)\n\
         Dirs referenced by agents: {agent_dirs:?}\n\
         Dirs listed in install.sh:  {listed:?}",
    );

    // Sanity check that the parser discovered both an inline agent dir (the
    // customer's OpenCode permission failure that motivated this guard) and a
    // helper-resolved dir (.claude via claude_config_dir() in utils.rs).
    assert!(
        agent_dirs.contains(".config/opencode"),
        "expected .config/opencode to be discovered from agent sources"
    );
    assert!(
        agent_dirs.contains(".claude"),
        "expected .claude (claude_config_dir) to be discovered; helper scanning may be broken"
    );
}

#[test]
fn install_sh_chown_loop_is_well_formed() {
    // Defensive: make sure install.sh still actually performs the chown, so a
    // refactor can't accidentally drop the loop while leaving the list behind.
    let install_sh = repo_root().join("install.sh");
    let source = fs::read_to_string(&install_sh).expect("read install.sh");
    assert!(
        source.contains("for agent_dir in")
            && source.contains(r#"chown -R "$INSTALL_USER" "$agent_dir""#),
        "install.sh no longer contains the agent-dir chown loop this test guards"
    );
}
