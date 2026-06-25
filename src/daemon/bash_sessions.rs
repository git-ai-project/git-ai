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
    pub start_trace_id: String,
    pub started_at_ns: u128,
    pub command: Option<String>,
    pub started_at: Instant,
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
}

impl BashSessionState {
    pub fn new() -> Self {
        Self::default()
    }

    fn prune_stale_sessions(&mut self) {
        self.sessions
            .retain(|_, s| s.started_at.elapsed() < Duration::from_secs(STALE_SESSION_SECS));
    }

    pub fn start_session(&mut self, session: BashSessionStart) {
        self.prune_stale_sessions();
        self.sessions.insert(
            (session.session_id, session.tool_use_id),
            BashSession {
                repo_work_dir: session.repo_work_dir,
                agent_id: session.agent_id,
                metadata: session.metadata,
                stat_snapshot: session.stat_snapshot,
                start_trace_id: session.start_trace_id,
                started_at_ns: session.started_at_ns,
                command: session.command,
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
