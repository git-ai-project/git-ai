use git_ai::core::post_commit::generate_authorship_for_commit;

use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::commands::helpers::{debug_log, discover_repo_and_gitdir, read_head_sha};

pub fn handle_post_commit() {
    // Discover repo root and git dir without spawning git
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (repo_dir, git_dir) = match discover_repo_and_gitdir(&cwd) {
        Some(pair) => pair,
        None => return,
    };

    // Read HEAD sha from filesystem (avoids a git spawn)
    let commit_sha = match read_head_sha(&git_dir) {
        Some(sha) => sha,
        None => return,
    };

    // Read parent SHA and author from the commit object (single git spawn)
    let output = match Command::new("/usr/bin/git")
        .args(["cat-file", "commit", &commit_sha])
        .current_dir(&repo_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return,
    };

    let mut parent_sha: Option<String> = None;
    let mut human_author = String::from("Unknown <unknown>");
    for line in output.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("parent ") {
            if parent_sha.is_none() {
                parent_sha = Some(rest.trim().to_string());
            }
        } else if let Some(rest) = line.strip_prefix("author ") {
            if let Some(email_end) = rest.find("> ") {
                human_author = rest[..email_end + 1].to_string();
            }
        }
    }
    let base_commit_owned = parent_sha.clone().unwrap_or_else(|| "initial".to_string());
    let base_commit = base_commit_owned.as_str();

    let (mut authorship_log, initial_attrs) = match generate_authorship_for_commit(
        &git_dir,
        &repo_dir,
        base_commit,
        &commit_sha,
        &human_author,
    ) {
        Ok(result) => result,
        Err(_) => return,
    };

    // Background cloud agent: when GIT_AI_CLOUD_AGENT=1 is set, attribute all
    // unattributed committed lines to AI. This covers no-hooks agents that don't
    // fire their own checkpoints.
    if env::var("GIT_AI_CLOUD_AGENT").as_deref() == Ok("1") {
        // Only apply on normal commits, not during rebase/cherry-pick
        let is_rewriting = git_dir.join("rebase-merge").exists()
            || git_dir.join("rebase-apply").exists()
            || git_dir.join("CHERRY_PICK_HEAD").exists();

        if !is_rewriting {
            let committed_lines =
                git_ai::core::post_commit::git_diff_committed_lines(&repo_dir, base_commit, &commit_sha);

            // Build a synthetic session ID for the background agent
            let bg_session_id =
                git_ai::core::authorship_log::generate_session_id("cloud-agent", &commit_sha);

            // Determine which committed lines are already attributed
            use std::collections::{HashMap as StdHashMap, HashSet as StdHashSet};
            let mut already_attributed: StdHashMap<&str, StdHashSet<u32>> = StdHashMap::new();
            for file_att in &authorship_log.attestations {
                let line_set = already_attributed
                    .entry(file_att.file_path.as_str())
                    .or_default();
                for entry in &file_att.entries {
                    for range in &entry.line_ranges {
                        match range {
                            git_ai::core::authorship_log::LineRange::Single(l) => {
                                line_set.insert(*l);
                            }
                            git_ai::core::authorship_log::LineRange::Range(s, e) => {
                                for l in *s..=*e {
                                    line_set.insert(l);
                                }
                            }
                        }
                    }
                }
            }

            // For each committed file, find unattributed lines and add them
            let mut bg_attestations: StdHashMap<String, Vec<u32>> = StdHashMap::new();
            for (file_path, lines) in &committed_lines {
                let attributed = already_attributed.get(file_path.as_str());
                for &line in lines {
                    let is_covered = attributed.map(|s| s.contains(&line)).unwrap_or(false);
                    if !is_covered {
                        bg_attestations
                            .entry(file_path.clone())
                            .or_default()
                            .push(line);
                    }
                }
            }

            // Add attestation entries for background agent lines
            if !bg_attestations.is_empty() {
                // Register the session in metadata
                authorship_log.metadata.sessions.insert(
                    bg_session_id.clone(),
                    git_ai::core::authorship_log::SessionRecord {
                        agent_id: git_ai::core::authorship_log::AgentId {
                            tool: "cloud-agent".to_string(),
                            id: commit_sha.clone(),
                            model: "unknown".to_string(),
                        },
                        human_author: Some(human_author.clone()),
                        custom_attributes: None,
                    },
                );

                for (file_path, mut lines) in bg_attestations {
                    lines.sort_unstable();
                    lines.dedup();
                    let ranges = git_ai::core::authorship_log::LineRange::compress_lines(&lines);

                    // Check if there's an existing attestation for this file
                    let existing = authorship_log
                        .attestations
                        .iter_mut()
                        .find(|fa| fa.file_path == file_path);

                    if let Some(file_att) = existing {
                        file_att.entries.push(git_ai::core::authorship_log::AttestationEntry {
                            hash: bg_session_id.clone(),
                            line_ranges: ranges,
                        });
                    } else {
                        authorship_log
                            .attestations
                            .push(git_ai::core::authorship_log::FileAttestation {
                                file_path,
                                entries: vec![git_ai::core::authorship_log::AttestationEntry {
                                    hash: bg_session_id.clone(),
                                    line_ranges: ranges,
                                }],
                            });
                    }
                }
            }
        }
    }

    let note_text = authorship_log.serialize_to_string();
    let result = Command::new("/usr/bin/git")
        .args([
            "notes",
            "--ref=ai",
            "add",
            "-f",
            "-m",
            &note_text,
            &commit_sha,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    match result {
        Ok(status) if status.success() => {
            debug_log(&format!(
                "wrote authorship note for {}",
                &commit_sha[..7.min(commit_sha.len())]
            ));
        }
        Ok(_) => debug_log("git notes add failed"),
        Err(e) => debug_log(&format!("failed to run git notes: {}", e)),
    }

    if let Some(initial) = initial_attrs {
        git_ai::core::working_log::write_initial_attributions(&git_dir, &commit_sha, &initial);
    }

    git_ai::core::working_log::delete_working_log(&git_dir, base_commit);
}
