use git_ai::core::authorship_log::AuthorshipLog;

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::process::{Command, Stdio};

use crate::commands::helpers::{debug_log, git_cmd};

pub fn handle_post_rewrite(args: &[String]) {
    // The post-rewrite hook receives old-sha new-sha pairs on stdin.
    // If --stdin is passed, read from stdin. Otherwise, try to infer from reflog.
    let use_stdin = args.iter().any(|a| a == "--stdin");

    let mappings: Vec<(String, String)> = if use_stdin {
        use std::io::BufRead;
        std::io::stdin()
            .lock()
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .collect()
    } else if args.len() >= 2 {
        // Direct old-sha new-sha pairs as arguments
        let mut pairs = Vec::new();
        let mut i = 0;
        let filtered: Vec<&String> = args
            .iter()
            .filter(|a| *a != "rebase" && *a != "amend")
            .collect();
        while i + 1 < filtered.len() {
            pairs.push((filtered[i].clone(), filtered[i + 1].clone()));
            i += 2;
        }
        pairs
    } else {
        Vec::new()
    };

    for (old_sha, new_sha) in &mappings {
        // Try to read the authorship note from the old commit
        let note = match git_cmd(&["notes", "--ref=ai", "show", old_sha]) {
            Ok(n) => n,
            Err(_) => continue,
        };

        if note.trim().is_empty() {
            continue;
        }

        // Update the base_commit_sha in the note metadata to point to the new commit
        let updated_note = if let Ok(mut log) = AuthorshipLog::deserialize_from_string(&note) {
            log.metadata.base_commit_sha = new_sha.clone();
            log.serialize_to_string()
        } else {
            note
        };

        // Write the note to the new commit
        let result = Command::new("/usr/bin/git")
            .args([
                "notes",
                "--ref=ai",
                "add",
                "-f",
                "-m",
                &updated_note,
                new_sha,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status();

        match result {
            Ok(status) if status.success() => {
                debug_log(&format!(
                    "copied authorship note {} -> {}",
                    &old_sha[..7.min(old_sha.len())],
                    &new_sha[..7.min(new_sha.len())]
                ));
            }
            _ => {
                debug_log(&format!(
                    "failed to copy note from {} to {}",
                    &old_sha[..7.min(old_sha.len())],
                    &new_sha[..7.min(new_sha.len())]
                ));
            }
        }
    }

    if mappings.is_empty() {
        debug_log("post-rewrite: no mappings provided");
    }
}

pub fn handle_post_rewrite_squash(args: &[String]) {
    use git_ai::core::attribution::{LineDiffOp, diff_slices};

    // Format: post-rewrite-squash <target_sha> <source1> <source2> ...
    // Merges all source notes into a single combined note on target_sha.
    if args.is_empty() {
        debug_log("post-rewrite-squash: no target SHA provided");
        return;
    }

    let target_sha = &args[0];
    let source_shas = &args[1..];

    if source_shas.is_empty() {
        debug_log("post-rewrite-squash: no source SHAs provided");
        return;
    }

    // Parse all source notes and collect metadata
    let mut parsed_notes: Vec<(String, AuthorshipLog)> = Vec::new();
    let mut all_files: HashSet<String> = HashSet::new();
    let mut merged_sessions: BTreeMap<String, git_ai::core::authorship_log::SessionRecord> =
        BTreeMap::new();
    let mut merged_humans: BTreeMap<String, git_ai::core::authorship_log::HumanRecord> =
        BTreeMap::new();
    let mut merged_prompts: BTreeMap<String, git_ai::core::authorship_log::PromptRecord> =
        BTreeMap::new();

    for source_sha in source_shas {
        debug_log(&format!(
            "post-rewrite-squash: looking up note for source {}",
            source_sha
        ));
        let note = match git_cmd(&["notes", "--ref=ai", "show", source_sha]) {
            Ok(n) => n,
            Err(e) => {
                debug_log(&format!(
                    "post-rewrite-squash: no note for {}: {}",
                    source_sha, e
                ));
                continue;
            }
        };
        if note.trim().is_empty() {
            continue;
        }

        let log = match AuthorshipLog::deserialize_from_string(&note) {
            Ok(l) => l,
            Err(_) => continue,
        };

        // Merge metadata
        for (id, session) in &log.metadata.sessions {
            merged_sessions
                .entry(id.clone())
                .or_insert_with(|| session.clone());
        }
        for (id, human) in &log.metadata.humans {
            merged_humans
                .entry(id.clone())
                .or_insert_with(|| human.clone());
        }
        for (id, prompt) in &log.metadata.prompts {
            merged_prompts
                .entry(id.clone())
                .or_insert_with(|| prompt.clone());
        }

        for att in &log.attestations {
            all_files.insert(att.file_path.clone());
        }
        parsed_notes.push((source_sha.clone(), log));
    }

    if parsed_notes.is_empty() {
        debug_log("post-rewrite-squash: no notes found in source commits");
        return;
    }

    // Sequential replay: for each file, accumulate attributions by diffing
    // consecutive commit contents and transferring line numbers forward.
    // accumulated_attrs: file_path -> (line_number -> author_hash)
    let mut accumulated_attrs: std::collections::HashMap<String, BTreeMap<u32, String>> =
        std::collections::HashMap::new();
    let mut prev_contents: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    let all_files_vec: Vec<String> = all_files.iter().cloned().collect();

    for (i, (source_sha, authorship_log)) in parsed_notes.iter().enumerate() {
        // Get file contents at this commit
        let mut current_contents: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for file_path in &all_files_vec {
            let spec = format!("{}:{}", source_sha, file_path);
            if let Ok(content) = git_cmd(&["show", &spec]) {
                current_contents.insert(file_path.clone(), content);
            }
        }

        if i > 0 {
            // Transfer accumulated attributions through diff
            for file_path in &all_files_vec {
                let prev_content = prev_contents.get(file_path);
                let curr_content = current_contents.get(file_path);

                if let (Some(prev_c), Some(curr_c)) = (prev_content, curr_content) {
                    if prev_c == curr_c {
                        continue; // No change, attributions stay the same
                    }
                    if let Some(attrs) = accumulated_attrs.get(file_path) {
                        if attrs.is_empty() {
                            continue;
                        }
                        // Diff old->new and transfer attributions
                        let old_lines: Vec<&str> = prev_c.lines().collect();
                        let new_lines: Vec<&str> = curr_c.lines().collect();
                        let ops = diff_slices(&old_lines, &new_lines);

                        let mut new_attrs: BTreeMap<u32, String> = BTreeMap::new();
                        for op in &ops {
                            if let LineDiffOp::Equal {
                                old_index,
                                new_index,
                                len,
                            } = op
                            {
                                for j in 0..*len {
                                    let old_line_num = (*old_index + j + 1) as u32;
                                    let new_line_num = (*new_index + j + 1) as u32;
                                    if let Some(hash) = attrs.get(&old_line_num) {
                                        new_attrs.insert(new_line_num, hash.clone());
                                    }
                                }
                            }
                        }
                        accumulated_attrs.insert(file_path.clone(), new_attrs);
                    }
                }
            }
        }

        // Overlay this commit's note attributions
        for file_attestation in &authorship_log.attestations {
            let file_path = &file_attestation.file_path;
            let entry = accumulated_attrs.entry(file_path.clone()).or_default();
            for att_entry in &file_attestation.entries {
                for line in att_entry.line_ranges.iter().flat_map(|r| r.expand()) {
                    entry.insert(line, att_entry.hash.clone());
                }
            }
        }

        prev_contents = current_contents;
    }

    // Final diff: last source commit content -> target commit content
    // (In fixup squash, these should be identical, but handle the general case)
    for file_path in &all_files_vec {
        let spec = format!("{}:{}", target_sha, file_path);
        if let Ok(target_content) = git_cmd(&["show", &spec])
            && let Some(prev_c) = prev_contents.get(file_path)
            && prev_c != &target_content
            && let Some(attrs) = accumulated_attrs.get(file_path)
            && !attrs.is_empty()
        {
            let old_lines: Vec<&str> = prev_c.lines().collect();
            let new_lines: Vec<&str> = target_content.lines().collect();
            let ops = diff_slices(&old_lines, &new_lines);

            let mut new_attrs: BTreeMap<u32, String> = BTreeMap::new();
            for op in &ops {
                if let LineDiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } = op
                {
                    for j in 0..*len {
                        let old_line_num = (*old_index + j + 1) as u32;
                        let new_line_num = (*new_index + j + 1) as u32;
                        if let Some(hash) = attrs.get(&old_line_num) {
                            new_attrs.insert(new_line_num, hash.clone());
                        }
                    }
                }
            }
            accumulated_attrs.insert(file_path.clone(), new_attrs);
        }
    }

    // Build merged attestations from accumulated attrs
    let mut merged_attestations: Vec<git_ai::core::authorship_log::FileAttestation> = Vec::new();
    for (file_path, attrs) in &accumulated_attrs {
        if attrs.is_empty() {
            continue;
        }
        // Group by hash
        let mut hash_lines: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for (line, hash) in attrs {
            hash_lines.entry(hash.clone()).or_default().push(*line);
        }
        let mut entries: Vec<git_ai::core::authorship_log::AttestationEntry> = Vec::new();
        for (hash, mut lines) in hash_lines {
            lines.sort();
            entries.push(git_ai::core::authorship_log::AttestationEntry {
                hash,
                line_ranges: git_ai::core::authorship_log::LineRange::compress_lines(&lines),
            });
        }
        merged_attestations.push(git_ai::core::authorship_log::FileAttestation {
            file_path: file_path.clone(),
            entries,
        });
    }

    if merged_attestations.is_empty()
        && merged_sessions.is_empty()
        && merged_humans.is_empty()
        && merged_prompts.is_empty()
    {
        debug_log("post-rewrite-squash: no notes found in source commits");
        return;
    }

    // Build the merged authorship log
    let merged_log = AuthorshipLog {
        attestations: merged_attestations,
        metadata: git_ai::core::authorship_log::Metadata {
            schema_version: "authorship/3.0.0".to_string(),
            git_ai_version: None,
            base_commit_sha: target_sha.clone(),
            prompts: merged_prompts,
            sessions: merged_sessions,
            humans: merged_humans,
        },
    };

    let merged_note = merged_log.serialize_to_string();

    // Write the merged note to the target commit
    let result = Command::new("/usr/bin/git")
        .args([
            "notes",
            "--ref=ai",
            "add",
            "-f",
            "-m",
            &merged_note,
            target_sha,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    match result {
        Ok(status) if status.success() => {
            debug_log(&format!(
                "post-rewrite-squash: merged {} source notes into {}",
                parsed_notes.len(),
                &target_sha[..7.min(target_sha.len())]
            ));
        }
        _ => {
            debug_log(&format!(
                "post-rewrite-squash: failed to write merged note to {}",
                &target_sha[..7.min(target_sha.len())]
            ));
        }
    }
}
