//! Post-commit attribution recovery.
//!
//! After the initial `AuthorshipLog` is built, some committed lines may still
//! have no attestation ("unknown"/untracked). This module runs an ordered
//! pipeline of [`RecoverySolver`]s that attribute those lines using
//! out-of-band signals (bash-checkpoint timing, adjacency to AI code, …).
//!
//! The shared [`unknown_lines`] helper computes, per file, the committed line
//! numbers that have no attestation entry. It is used both here and by
//! `background_agent` so the two stay in lock-step.

use crate::authorship::authorship_log::{LineRange, SessionRecord};
use crate::authorship::authorship_log_serialization::{AttestationEntry, AuthorshipLog};
use crate::authorship::working_log::AgentId;
use crate::git::repository::Repository;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub mod bash_solver;
pub mod edge_solver;

/// Per-file committed line numbers that have no attestation entry ("unknown").
///
/// `committed_hunks` is the set of added lines per file in the commit; any line
/// in there that is not covered by an existing `AttestationEntry` is returned
/// (sorted, deduped). Files with no unknown lines are omitted from the result.
pub fn unknown_lines(
    authorship_log: &AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) -> HashMap<String, Vec<u32>> {
    let mut attributed: HashMap<&str, HashSet<u32>> = HashMap::new();
    for fa in &authorship_log.attestations {
        let set = attributed.entry(fa.file_path.as_str()).or_default();
        for entry in &fa.entries {
            for range in &entry.line_ranges {
                for line in range.expand() {
                    set.insert(line);
                }
            }
        }
    }

    let mut out = HashMap::new();
    for (file, ranges) in committed_hunks {
        let existing = attributed.get(file.as_str());
        let mut unknown: Vec<u32> = Vec::new();
        for range in ranges {
            for line in range.expand() {
                if existing.is_none_or(|s| !s.contains(&line)) {
                    unknown.push(line);
                }
            }
        }
        if !unknown.is_empty() {
            unknown.sort_unstable();
            unknown.dedup();
            out.insert(file.clone(), unknown);
        }
    }
    out
}

/// Context handed to every solver. `repo` is optional — the timing/edge solvers
/// only need `repo_work_dir` + `committed_hunks`; only callers with a live repo
/// (post-commit) populate it.
pub struct RecoveryContext<'a> {
    pub repo: Option<&'a Repository>,
    pub commit_sha: &'a str,
    pub parent_sha: &'a str,
    pub repo_work_dir: &'a Path,
    pub committed_hunks: &'a HashMap<String, Vec<LineRange>>,
    pub human_author: &'a str,
}

impl<'a> RecoveryContext<'a> {
    #[cfg(test)]
    pub fn for_test(
        repo_work_dir: &'a Path,
        committed_hunks: &'a HashMap<String, Vec<LineRange>>,
        human_author: &'a str,
    ) -> Self {
        Self {
            repo: None,
            commit_sha: "test_commit",
            parent_sha: "test_parent",
            repo_work_dir,
            committed_hunks,
            human_author,
        }
    }
}

/// The AI owner of a single committed line, used by the edge-extension solver.
#[derive(Debug, Clone)]
pub struct AiLineOwner {
    pub session_key: String,
    pub agent_id: AgentId,
    pub edit_kind: String,
}

/// A checkpoint metric event to emit for a recovered attribution.
#[derive(Debug, Clone)]
pub struct RecoveredCheckpointMetric {
    pub session_key: String,
    pub trace_id: String,
    pub agent_id: AgentId,
    pub file_path: String,
    pub lines_added: u32,
    pub edit_kind: String,
    pub checkpoint_ts: u64,
    pub recovery_metadata_json: String,
}

/// A solver's proposal: cover `per_file_lines` for a session and emit `metrics`.
#[derive(Debug, Clone)]
pub struct RecoveredAttribution {
    pub session_key: String,
    pub trace_id: String,
    pub session_record: SessionRecord,
    pub per_file_lines: HashMap<String, Vec<LineRange>>,
    pub metrics: Vec<RecoveredCheckpointMetric>,
}

/// A pluggable attribution-recovery stage.
pub trait RecoverySolver {
    fn name(&self) -> &'static str;
    /// Propose attributions for currently-unknown lines. `ai_lines` maps each
    /// file to the AI owner of each already-AI-attributed committed line.
    fn solve(
        &self,
        ctx: &RecoveryContext,
        unknown: &HashMap<String, Vec<u32>>,
        ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>,
    ) -> Vec<RecoveredAttribution>;
}

/// Apply a solver's proposal to the authorship log: register the session and
/// add an attestation entry (hash `s_...::t_...`) per file.
///
/// The session record is inserted only if the key is new. The edge-extension
/// solver deliberately reuses an existing AI session's key (with a fresh trace
/// id); overwriting that session's record would clobber its real `human_author`
/// and `custom_attributes`, so we preserve the original via `or_insert_with`.
pub fn apply_recovered(log: &mut AuthorshipLog, rec: &RecoveredAttribution) {
    log.metadata
        .sessions
        .entry(rec.session_key.clone())
        .or_insert_with(|| rec.session_record.clone());
    let hash = format!("{}::{}", rec.session_key, rec.trace_id);
    for (file, ranges) in &rec.per_file_lines {
        let fa = log.get_or_create_file(file);
        fa.add_entry(AttestationEntry::new(hash.clone(), ranges.clone()));
    }
}

/// Cheap pre-check: could any solver possibly recover something for this commit?
///
/// Returns true only if the edge solver has AI attestations to extend from, or
/// the bash solver has at least one recorded bash checkpoint for this repo.
/// Lets callers skip the (process-spawning) committed-hunks diff on the common
/// path — a fully human-attributed commit in a repo with no bash checkpoints.
pub fn recovery_possible(log: &AuthorshipLog, repo_work_dir: &Path) -> bool {
    // Edge solver: needs at least one AI (non-`h_`) attestation in the log.
    let has_ai = log.attestations.iter().any(|fa| {
        fa.entries.iter().any(|e| {
            let key = e.hash.split("::").next().unwrap_or(&e.hash);
            !key.starts_with("h_") && ai_owner_for(log, key).is_some()
        })
    });
    if has_ai {
        return true;
    }
    // Bash solver: needs a recorded bash checkpoint for this repo.
    let repo_key = std::fs::canonicalize(repo_work_dir)
        .unwrap_or_else(|_| repo_work_dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    crate::daemon::bash_checkpoints_db::BashCheckpointsDatabase::global()
        .ok()
        .and_then(|db| {
            db.lock()
                .ok()
                .map(|g| g.has_rows_for_repo(&repo_key).unwrap_or(false))
        })
        .unwrap_or(false)
}

/// Map each file's already-AI-attributed committed lines to their AI owner.
///
/// An attestation hash is AI iff its session-key part (before `::`, if present)
/// is not a known-human hash (`h_` prefix) and resolves in `metadata.sessions`
/// or `metadata.prompts` to a non-human agent.
pub fn ai_lines_from_log(log: &AuthorshipLog) -> HashMap<String, HashMap<u32, AiLineOwner>> {
    let mut out: HashMap<String, HashMap<u32, AiLineOwner>> = HashMap::new();
    for fa in &log.attestations {
        for entry in &fa.entries {
            let session_key = entry.hash.split("::").next().unwrap_or(&entry.hash);
            if session_key.starts_with("h_") {
                continue;
            }
            let Some((agent_id, edit_kind)) = ai_owner_for(log, session_key) else {
                continue;
            };
            let file_map = out.entry(fa.file_path.clone()).or_default();
            for range in &entry.line_ranges {
                for line in range.expand() {
                    file_map.insert(
                        line,
                        AiLineOwner {
                            session_key: session_key.to_string(),
                            agent_id: agent_id.clone(),
                            edit_kind: edit_kind.clone(),
                        },
                    );
                }
            }
        }
    }
    out
}

/// Resolve a session key to its AI agent identity, if it names an AI session.
fn ai_owner_for(log: &AuthorshipLog, session_key: &str) -> Option<(AgentId, String)> {
    if let Some(sr) = log.metadata.sessions.get(session_key) {
        return Some((sr.agent_id.clone(), "file_edit".to_string()));
    }
    if let Some(pr) = log.metadata.prompts.get(session_key) {
        return Some((pr.agent_id.clone(), "file_edit".to_string()));
    }
    None
}

/// Drop, per file, unknown line numbers whose committed content already appears
/// in the parent commit's version of that file. Such lines were not produced by
/// this commit (they are unknown only as a diff-base artifact) and must not be
/// recovered. No-op when there is no repo context (unit tests) or no parent
/// (`initial`); files absent from the parent keep all their lines.
fn retain_lines_new_vs_parent(ctx: &RecoveryContext, unknown: &mut HashMap<String, Vec<u32>>) {
    let Some(repo) = ctx.repo else {
        return;
    };
    if ctx.parent_sha == "initial" || ctx.parent_sha.is_empty() {
        return;
    }
    let mut requests: Vec<(String, String)> = Vec::with_capacity(unknown.len() * 2);
    for file in unknown.keys() {
        requests.push((ctx.parent_sha.to_string(), file.clone()));
        requests.push((ctx.commit_sha.to_string(), file.clone()));
    }
    let Ok(contents) = crate::git::repository::batch_read_paths_at_treeishes(repo, &requests)
    else {
        return;
    };
    unknown.retain(|file, lines| {
        let Some(committed) = contents.get(&(ctx.commit_sha.to_string(), file.clone())) else {
            return true;
        };
        // File absent from the parent ⇒ all lines are new; keep them.
        let Some(parent) = contents.get(&(ctx.parent_sha.to_string(), file.clone())) else {
            return true;
        };
        // A committed line is eligible only if its content does not already exist
        // in the parent version of the file. This is a deliberately conservative
        // bag-of-lines test: it can under-recover a genuinely-new line that is
        // textually identical to some unrelated parent line (e.g. a lone `}`),
        // but it never mis-attributes pre-existing content. We accept the
        // occasional false-negative because over-attribution (claiming a human's
        // carried-forward line as AI) is far worse than missing one bare line.
        let parent_line_set: HashSet<&str> = parent.lines().collect();
        let committed_lines: Vec<&str> = committed.lines().collect();
        lines.retain(|&ln| {
            committed_lines
                .get((ln as usize).saturating_sub(1))
                .is_none_or(|content| !parent_line_set.contains(content))
        });
        !lines.is_empty()
    });
}

/// Run the ordered solver pipeline. Each solver sees the current unknown set and
/// AI-line map; its proposals are applied before the next solver runs. Returns
/// the collected metrics for emission. Solver panics are isolated and skipped.
pub fn recover_attribution(
    log: &mut AuthorshipLog,
    ctx: &RecoveryContext,
    solvers: &[Box<dyn RecoverySolver>],
) -> Vec<RecoveredCheckpointMetric> {
    let mut all_metrics = Vec::new();
    for solver in solvers {
        let mut unknown = unknown_lines(log, ctx.committed_hunks);
        // Recovery only attributes lines this commit genuinely *produced*. Drop
        // unknown lines that are not insertions in the parent→commit diff
        // (pre-existing content that is unknown-in-this-commit only as a diff-base
        // artifact, e.g. a prior line carried forward into an in-flight commit).
        // Applies to every solver so neither bash correlation nor edge extension
        // sweeps up unrelated pre-existing lines. New files have no parent
        // version, so all their lines remain eligible.
        retain_lines_new_vs_parent(ctx, &mut unknown);
        if unknown.is_empty() {
            break;
        }
        let ai_lines = ai_lines_from_log(log);
        let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            solver.solve(ctx, &unknown, &ai_lines)
        }))
        .unwrap_or_else(|_| {
            tracing::warn!("recovery solver {} panicked; skipping", solver.name());
            vec![]
        });
        for rec in recovered {
            apply_recovered(log, &rec);
            all_metrics.extend(rec.metrics);
        }
    }
    all_metrics
}

/// Emit one `recovered` checkpoint metric event per recovered file.
pub fn emit_recovered_metrics(
    _repo: &Repository,
    _commit_sha: &str,
    _parent_sha: &str,
    metrics: &[RecoveredCheckpointMetric],
) {
    for m in metrics {
        let values = crate::metrics::CheckpointValues::new()
            .checkpoint_ts(m.checkpoint_ts)
            .kind("ai_agent")
            .file_path(m.file_path.clone())
            .lines_added(m.lines_added)
            .edit_kind(m.edit_kind.clone())
            .attribution_recovery_metadata(m.recovery_metadata_json.clone());
        let attrs = crate::metrics::EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
            .session_id(m.session_key.clone())
            .trace_id(m.trace_id.clone())
            .tool(m.agent_id.tool.clone())
            .model(m.agent_id.model.clone());
        crate::metrics::record(values, attrs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::authorship_log_serialization::{AttestationEntry, AuthorshipLog};
    use std::collections::HashMap;

    #[test]
    fn test_unknown_lines_subtracts_attributed() {
        let mut log = AuthorshipLog::new();
        log.get_or_create_file("a.rs")
            .add_entry(AttestationEntry::new(
                "hash1".into(),
                vec![LineRange::Range(1, 3)],
            ));
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 5)]);
        let unknown = unknown_lines(&log, &committed);
        assert_eq!(unknown.get("a.rs").unwrap(), &vec![4, 5]);
    }

    #[test]
    fn test_unknown_lines_all_attributed_omits_file() {
        let mut log = AuthorshipLog::new();
        log.get_or_create_file("a.rs")
            .add_entry(AttestationEntry::new(
                "hash1".into(),
                vec![LineRange::Range(1, 5)],
            ));
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 5)]);
        let unknown = unknown_lines(&log, &committed);
        assert!(unknown.is_empty());
    }
}

#[cfg(test)]
mod orch_tests {
    use super::*;
    use crate::authorship::authorship_log::{LineRange, SessionRecord};
    use crate::authorship::authorship_log_serialization::AuthorshipLog;
    use crate::authorship::working_log::AgentId;
    use std::collections::HashMap;
    use std::path::Path;

    struct CoverAllSolver;
    impl RecoverySolver for CoverAllSolver {
        fn name(&self) -> &'static str {
            "cover_all"
        }
        fn solve(
            &self,
            _ctx: &RecoveryContext,
            unknown: &HashMap<String, Vec<u32>>,
            _ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>,
        ) -> Vec<RecoveredAttribution> {
            if unknown.is_empty() {
                return vec![];
            }
            let agent = AgentId {
                tool: "bash".into(),
                id: "x".into(),
                model: "m".into(),
            };
            let session_key = "s_test".to_string();
            let mut per_file = HashMap::new();
            let mut metrics = vec![];
            for (f, lines) in unknown {
                per_file.insert(f.clone(), LineRange::compress_lines(lines));
                metrics.push(RecoveredCheckpointMetric {
                    session_key: session_key.clone(),
                    trace_id: "t_test".into(),
                    agent_id: agent.clone(),
                    file_path: f.clone(),
                    lines_added: lines.len() as u32,
                    edit_kind: "bash".into(),
                    checkpoint_ts: 1,
                    recovery_metadata_json: "{}".into(),
                });
            }
            vec![RecoveredAttribution {
                session_key,
                trace_id: "t_test".into(),
                session_record: SessionRecord {
                    agent_id: agent,
                    human_author: None,
                    custom_attributes: None,
                },
                per_file_lines: per_file,
                metrics,
            }]
        }
    }

    #[test]
    fn test_recover_attribution_covers_unknown() {
        let mut log = AuthorshipLog::new();
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 2)]);
        let ctx = RecoveryContext::for_test(Path::new("/repo"), &committed, "h");
        let solvers: Vec<Box<dyn RecoverySolver>> = vec![Box::new(CoverAllSolver)];
        let metrics = recover_attribution(&mut log, &ctx, &solvers);
        assert_eq!(metrics.len(), 1);
        let fa = log
            .attestations
            .iter()
            .find(|f| f.file_path == "a.rs")
            .unwrap();
        assert!(!fa.entries.is_empty());
        assert!(log.metadata.sessions.contains_key("s_test"));
    }

    #[test]
    fn test_recover_attribution_stops_when_no_unknown() {
        // A fully-attributed log yields no unknown lines, so the solver never runs.
        let mut log = AuthorshipLog::new();
        log.get_or_create_file("a.rs").add_entry(
            crate::authorship::authorship_log_serialization::AttestationEntry::new(
                "hash1".into(),
                vec![LineRange::Range(1, 2)],
            ),
        );
        let mut committed = HashMap::new();
        committed.insert("a.rs".to_string(), vec![LineRange::Range(1, 2)]);
        let ctx = RecoveryContext::for_test(Path::new("/repo"), &committed, "h");
        let solvers: Vec<Box<dyn RecoverySolver>> = vec![Box::new(CoverAllSolver)];
        let metrics = recover_attribution(&mut log, &ctx, &solvers);
        assert!(metrics.is_empty());
    }
}
