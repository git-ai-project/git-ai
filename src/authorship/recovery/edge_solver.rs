//! Solver 2: extend existing AI attribution into directly-adjacent unknown
//! lines.
//!
//! Iterative AI work plus Git's Myers diff frequently leaves a stray unknown
//! line (a blank line, a brace, a trailing newline) immediately above or below
//! an AI-attributed block. This solver absorbs each maximal run of unknown
//! lines into the AI session that owns the line directly above or below the
//! run. It carries over the adjacent block's session id but mints a fresh trace
//! id so the recovery is distinguishable. It never touches lines adjacent only
//! to human/known-human code.

use crate::authorship::authorship_log::{LineRange, SessionRecord};
use crate::authorship::authorship_log_serialization::generate_trace_id;
use crate::authorship::recovery::{
    AiLineOwner, RecoveredAttribution, RecoveredCheckpointMetric, RecoveryContext, RecoverySolver,
};
use std::collections::HashMap;

pub struct AiEdgeExtensionSolver;

impl RecoverySolver for AiEdgeExtensionSolver {
    fn name(&self) -> &'static str {
        "ai_edge_extension"
    }

    fn solve(
        &self,
        _ctx: &RecoveryContext,
        unknown: &HashMap<String, Vec<u32>>,
        ai_lines: &HashMap<String, HashMap<u32, AiLineOwner>>,
    ) -> Vec<RecoveredAttribution> {
        let mut out = Vec::new();
        for (file, lines) in unknown {
            let Some(ai_map) = ai_lines.get(file) else {
                continue;
            };
            if ai_map.is_empty() {
                continue;
            }
            for run in contiguous_runs(lines) {
                let above = run.first().copied().unwrap().saturating_sub(1);
                let below = run.last().copied().unwrap() + 1;
                // Prefer the block above; fall back to below.
                let (owner, side) = if let Some(o) = ai_map.get(&above) {
                    (o, "above")
                } else if let Some(o) = ai_map.get(&below) {
                    (o, "below")
                } else {
                    continue;
                };

                let trace_id = generate_trace_id();
                let mut per_file = HashMap::new();
                per_file.insert(file.clone(), LineRange::compress_lines(&run));
                let recovery_json = serde_json::json!({
                    "solver": "ai_edge_extension",
                    "extended_from_session": owner.session_key,
                    "adjacent_side": side,
                    "run_lines": run.len(),
                })
                .to_string();
                out.push(RecoveredAttribution {
                    session_key: owner.session_key.clone(),
                    trace_id: trace_id.clone(),
                    session_record: SessionRecord {
                        agent_id: owner.agent_id.clone(),
                        human_author: None,
                        custom_attributes: None,
                    },
                    per_file_lines: per_file,
                    metrics: vec![RecoveredCheckpointMetric {
                        session_key: owner.session_key.clone(),
                        trace_id,
                        agent_id: owner.agent_id.clone(),
                        file_path: file.clone(),
                        lines_added: run.len() as u32,
                        edit_kind: owner.edit_kind.clone(),
                        checkpoint_ts: 0,
                        recovery_metadata_json: recovery_json,
                    }],
                });
            }
        }
        out
    }
}

/// Split sorted line numbers into maximal contiguous runs.
fn contiguous_runs(sorted: &[u32]) -> Vec<Vec<u32>> {
    let mut runs = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    for &l in sorted {
        if cur.last().is_some_and(|&p| l != p + 1) {
            runs.push(std::mem::take(&mut cur));
        }
        cur.push(l);
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::recovery::RecoveryContext;
    use crate::authorship::working_log::AgentId;
    use std::collections::HashMap;
    use std::path::Path;

    fn owner(sk: &str) -> AiLineOwner {
        AiLineOwner {
            session_key: sk.into(),
            agent_id: AgentId {
                tool: "claude".into(),
                id: "x".into(),
                model: "m".into(),
            },
            edit_kind: "file_edit".into(),
        }
    }

    #[test]
    fn test_edge_extension_absorbs_adjacent_below() {
        let mut ai_map: HashMap<u32, AiLineOwner> = HashMap::new();
        for l in 1..=3 {
            ai_map.insert(l, owner("s_ai"));
        }
        let ai_lines: HashMap<String, HashMap<u32, AiLineOwner>> =
            [("a.rs".to_string(), ai_map)].into_iter().collect();
        let committed = HashMap::new();
        let ctx = RecoveryContext::for_test(Path::new("/r"), &committed, "h");
        // line 4 is unknown and directly below the AI block → absorbed
        let unknown: HashMap<String, Vec<u32>> =
            [("a.rs".to_string(), vec![4u32])].into_iter().collect();

        let recovered = AiEdgeExtensionSolver.solve(&ctx, &unknown, &ai_lines);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].session_key, "s_ai");
        assert!(
            recovered[0].per_file_lines["a.rs"]
                .iter()
                .any(|r| r.expand().contains(&4))
        );
    }

    #[test]
    fn test_edge_extension_skips_non_adjacent() {
        let ai_map: HashMap<u32, AiLineOwner> = [(1u32, owner("s_ai"))].into_iter().collect();
        let ai_lines: HashMap<String, HashMap<u32, AiLineOwner>> =
            [("a.rs".to_string(), ai_map)].into_iter().collect();
        let committed = HashMap::new();
        let ctx = RecoveryContext::for_test(Path::new("/r"), &committed, "h");
        // unknown line 10 is far from AI line 1 → untouched
        let unknown: HashMap<String, Vec<u32>> =
            [("a.rs".to_string(), vec![10u32])].into_iter().collect();
        assert!(
            AiEdgeExtensionSolver
                .solve(&ctx, &unknown, &ai_lines)
                .is_empty()
        );
    }

    #[test]
    fn test_edge_extension_no_steal_from_human_only() {
        // No AI lines for the file → nothing absorbed.
        let ai_lines: HashMap<String, HashMap<u32, AiLineOwner>> = HashMap::new();
        let committed = HashMap::new();
        let ctx = RecoveryContext::for_test(Path::new("/r"), &committed, "h");
        let unknown: HashMap<String, Vec<u32>> =
            [("a.rs".to_string(), vec![2u32])].into_iter().collect();
        assert!(
            AiEdgeExtensionSolver
                .solve(&ctx, &unknown, &ai_lines)
                .is_empty()
        );
    }

    #[test]
    fn test_edge_extension_absorbs_run_between_ai_blocks() {
        // AI owns 1 and 4; unknown run 2-3 sits between them → absorbed (above wins).
        let mut ai_map: HashMap<u32, AiLineOwner> = HashMap::new();
        ai_map.insert(1, owner("s_ai"));
        ai_map.insert(4, owner("s_other"));
        let ai_lines: HashMap<String, HashMap<u32, AiLineOwner>> =
            [("a.rs".to_string(), ai_map)].into_iter().collect();
        let committed = HashMap::new();
        let ctx = RecoveryContext::for_test(Path::new("/r"), &committed, "h");
        let unknown: HashMap<String, Vec<u32>> = [("a.rs".to_string(), vec![2u32, 3u32])]
            .into_iter()
            .collect();
        let recovered = AiEdgeExtensionSolver.solve(&ctx, &unknown, &ai_lines);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].session_key, "s_ai");
        let covered: Vec<u32> = recovered[0].per_file_lines["a.rs"]
            .iter()
            .flat_map(|r| r.expand())
            .collect();
        assert!(covered.contains(&2) && covered.contains(&3));
    }
}
