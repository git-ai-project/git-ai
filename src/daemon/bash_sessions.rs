use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::StatSnapshot;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const STALE_SESSION_SECS: u64 = 300;

pub struct BashSession {
    pub repo_work_dir: String,
    pub agent_id: AgentId,
    pub metadata: HashMap<String, String>,
    pub stat_snapshot: StatSnapshot,
    pub started_at: Instant,
}

#[derive(Default)]
pub struct BashSessionState {
    sessions: HashMap<(String, String), BashSession>,
}

impl BashSessionState {
    pub fn new() -> Self {
        Self::default()
    }

    fn prune_stale_sessions(&mut self) {
        self.sessions
            .retain(|_, s| s.started_at.elapsed() < Duration::from_secs(STALE_SESSION_SECS));
    }

    pub fn start_session(
        &mut self,
        session_id: String,
        tool_use_id: String,
        repo_work_dir: String,
        agent_id: AgentId,
        metadata: HashMap<String, String>,
        stat_snapshot: StatSnapshot,
    ) {
        self.prune_stale_sessions();
        self.sessions.insert(
            (session_id, tool_use_id),
            BashSession {
                repo_work_dir,
                agent_id,
                metadata,
                stat_snapshot,
                started_at: Instant::now(),
            },
        );
    }

    pub fn end_session(&mut self, session_id: &str, tool_use_id: &str) -> Option<BashSession> {
        self.sessions
            .remove(&(session_id.to_string(), tool_use_id.to_string()))
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
}
