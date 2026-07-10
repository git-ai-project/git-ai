use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::StatSnapshot;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const STALE_SESSION_SECS: u64 = 300;
const MAX_ACTIVE_BASH_SESSIONS: usize = 32;
const MAX_RETAINED_BASH_SESSION_BYTES: usize = 32 * 1024 * 1024;

pub struct BashSession {
    pub repo_work_dir: String,
    pub agent_id: AgentId,
    pub metadata: HashMap<String, String>,
    pub stat_snapshot: StatSnapshot,
    pub start_trace_id: String,
    pub started_at_ns: u128,
    pub command: Option<String>,
    pub started_at: Instant,
    retained_bytes: usize,
}

pub struct BashSessionStart {
    pub session_id: String,
    pub tool_use_id: String,
    pub repo_work_dir: String,
    pub agent_id: AgentId,
    pub metadata: HashMap<String, String>,
    pub stat_snapshot: StatSnapshot,
    pub start_trace_id: String,
    pub started_at_ns: u128,
    pub command: Option<String>,
}

#[derive(Default)]
pub struct BashSessionState {
    sessions: HashMap<(String, String), BashSession>,
    retained_bytes: usize,
}

impl BashSessionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn prune_stale_sessions(&mut self) {
        let mut removed_bytes = 0usize;
        self.sessions.retain(|_, session| {
            let keep = session.started_at.elapsed() < Duration::from_secs(STALE_SESSION_SECS);
            if !keep {
                removed_bytes = removed_bytes.saturating_add(session.retained_bytes);
            }
            keep
        });
        self.retained_bytes = self.retained_bytes.saturating_sub(removed_bytes);
    }

    pub fn start_session(&mut self, session: BashSessionStart) -> bool {
        self.prune_stale_sessions();
        let retained_bytes = retained_session_bytes(&session);
        if retained_bytes > MAX_RETAINED_BASH_SESSION_BYTES {
            return false;
        }

        let key = (session.session_id, session.tool_use_id);
        self.remove_session(&key);
        while self.sessions.len() >= MAX_ACTIVE_BASH_SESSIONS
            || self.retained_bytes.saturating_add(retained_bytes) > MAX_RETAINED_BASH_SESSION_BYTES
        {
            let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.started_at)
                .map(|(key, _)| key.clone())
            else {
                return false;
            };
            self.remove_session(&oldest);
        }

        self.retained_bytes = self.retained_bytes.saturating_add(retained_bytes);
        self.sessions.insert(
            key,
            BashSession {
                repo_work_dir: session.repo_work_dir,
                agent_id: session.agent_id,
                metadata: session.metadata,
                stat_snapshot: session.stat_snapshot,
                start_trace_id: session.start_trace_id,
                started_at_ns: session.started_at_ns,
                command: session.command,
                started_at: Instant::now(),
                retained_bytes,
            },
        );
        true
    }

    pub fn end_session(&mut self, session_id: &str, tool_use_id: &str) -> Option<BashSession> {
        self.remove_session(&(session_id.to_string(), tool_use_id.to_string()))
    }

    pub fn query_active_for_repo(
        &self,
        repo_work_dir: &str,
    ) -> Option<(&(String, String), &BashSession)> {
        self.sessions
            .iter()
            .find(|(_, s)| s.repo_work_dir == repo_work_dir)
    }

    pub fn get_snapshot(&self, session_id: &str, tool_use_id: &str) -> Option<&StatSnapshot> {
        self.sessions
            .get(&(session_id.to_string(), tool_use_id.to_string()))
            .map(|s| &s.stat_snapshot)
    }

    fn remove_session(&mut self, key: &(String, String)) -> Option<BashSession> {
        let removed = self.sessions.remove(key)?;
        self.retained_bytes = self.retained_bytes.saturating_sub(removed.retained_bytes);
        Some(removed)
    }
}

fn retained_session_bytes(session: &BashSessionStart) -> usize {
    let snapshot = &session.stat_snapshot;
    let mut bytes = std::mem::size_of::<BashSession>()
        .saturating_add(session.session_id.len())
        .saturating_add(session.tool_use_id.len())
        .saturating_add(session.repo_work_dir.len())
        .saturating_add(session.agent_id.tool.len())
        .saturating_add(session.agent_id.id.len())
        .saturating_add(session.agent_id.model.len())
        .saturating_add(session.start_trace_id.len())
        .saturating_add(session.command.as_ref().map_or(0, String::len))
        .saturating_add(snapshot.invocation_key.len())
        .saturating_add(snapshot.repo_root.to_string_lossy().len());
    for (key, value) in &session.metadata {
        bytes = bytes
            .saturating_add(std::mem::size_of::<String>() * 2)
            .saturating_add(key.len())
            .saturating_add(value.len());
    }
    for path in snapshot.entries.keys() {
        bytes = bytes
            .saturating_add(std::mem::size_of::<std::path::PathBuf>())
            .saturating_add(path.to_string_lossy().len())
            .saturating_add(std::mem::size_of::<
                crate::commands::checkpoint_agent::bash_tool::StatEntry,
            >());
    }
    for path in snapshot.per_file_wm.keys() {
        bytes = bytes
            .saturating_add(std::mem::size_of::<String>())
            .saturating_add(path.len())
            .saturating_add(std::mem::size_of::<u128>());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn start(index: usize) -> BashSessionStart {
        BashSessionStart {
            session_id: format!("session-{index}"),
            tool_use_id: format!("tool-{index}"),
            repo_work_dir: "/repo".to_string(),
            agent_id: AgentId {
                tool: "codex".to_string(),
                id: "test".to_string(),
                model: "test".to_string(),
            },
            metadata: HashMap::new(),
            stat_snapshot: StatSnapshot {
                entries: HashMap::new(),
                taken_at: None,
                invocation_key: format!("session-{index}:tool-{index}"),
                repo_root: "/repo".into(),
                effective_worktree_wm: None,
                per_file_wm: HashMap::new(),
            },
            start_trace_id: format!("trace-{index}"),
            started_at_ns: index as u128,
            command: None,
        }
    }

    #[test]
    fn active_bash_sessions_are_bounded() {
        const EXPECTED_LIMIT: usize = 32;

        let mut state = BashSessionState::new();
        for index in 0..=EXPECTED_LIMIT {
            state.start_session(start(index));
        }

        assert_eq!(state.sessions.len(), EXPECTED_LIMIT);
        assert!(state.get_snapshot("session-0", "tool-0").is_none());
        assert!(
            state
                .get_snapshot(
                    &format!("session-{EXPECTED_LIMIT}"),
                    &format!("tool-{EXPECTED_LIMIT}")
                )
                .is_some()
        );
    }

    #[test]
    fn oversized_bash_session_is_not_retained() {
        let mut state = BashSessionState::new();
        let mut session = start(1);
        session.metadata.insert(
            "oversized".to_string(),
            "x".repeat(MAX_RETAINED_BASH_SESSION_BYTES + 1),
        );

        assert!(!state.start_session(session));
        assert!(state.sessions.is_empty());
        assert_eq!(state.retained_bytes, 0);
    }
}
