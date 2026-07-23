use crate::authorship::authorship_log_serialization::{generate_session_id, generate_trace_id};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use crate::commands::checkpoint_agent::presets::ParsedHookEvent;
use crate::daemon::checkpoint::PreparedPathRole;
use crate::error::GitAiError;
use crate::metrics::{CheckpointValues, EventAttributes, MetricEvent, PosEncoded};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FORMAT_VERSION: u8 = 1;
const MAX_FILES_PER_BATCH: usize = 1_000;
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const STALE_TEMP_FILE_AGE: Duration = Duration::from_secs(60);
const SANDBOXED_CHECKPOINT_DIR: &str = "/tmp/git-ai-sandboxed-checkpoints";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxedCheckpointKind {
    Bash,
    FileEdit,
}

impl SandboxedCheckpointKind {
    fn edit_kind(self) -> &'static str {
        match self {
            Self::Bash => "bash_sandboxed",
            Self::FileEdit => "file_edit_sandboxed",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxedCheckpointPhase {
    Pre,
    Post,
}

impl SandboxedCheckpointPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Post => "post",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SandboxedCheckpointRecord {
    pub version: u8,
    pub timestamp_ns: u128,
    pub trace_id: String,
    pub preset: String,
    pub kind: SandboxedCheckpointKind,
    pub phase: SandboxedCheckpointPhase,
    pub agent_id: AgentId,
    pub external_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub file_paths: Vec<PathBuf>,
}

pub fn capture(preset: &str, hook_input: &str) -> Result<Vec<PathBuf>, GitAiError> {
    let records = records_to_capture(preset, hook_input)?;
    records.iter().map(write_record).collect()
}

pub fn capture_unsent_requests(
    preset: &str,
    hook_input: &str,
    requests: &[CheckpointRequest],
) -> Result<Vec<PathBuf>, GitAiError> {
    let templates = records_to_capture(preset, hook_input)?;
    records_for_requests(&templates, requests)
        .iter()
        .map(write_record)
        .collect()
}

fn records_to_capture(
    preset: &str,
    hook_input: &str,
) -> Result<Vec<SandboxedCheckpointRecord>, GitAiError> {
    let trace_id = generate_trace_id();
    let events = crate::commands::checkpoint_agent::presets::resolve_preset(preset)?
        .parse(hook_input, &trace_id)?;
    Ok(events
        .into_iter()
        .filter_map(|event| SandboxedCheckpointRecord::from_event(preset, event))
        .collect())
}

fn records_for_requests(
    templates: &[SandboxedCheckpointRecord],
    requests: &[CheckpointRequest],
) -> Vec<SandboxedCheckpointRecord> {
    requests
        .iter()
        .filter_map(|request| {
            let phase = match request.path_role {
                PreparedPathRole::WillEdit => SandboxedCheckpointPhase::Pre,
                PreparedPathRole::Edited => SandboxedCheckpointPhase::Post,
            };
            let kind = if request.metadata.get("edit_kind").map(String::as_str) == Some("bash")
                || (phase == SandboxedCheckpointPhase::Pre && request.agent_id.is_none())
            {
                SandboxedCheckpointKind::Bash
            } else {
                SandboxedCheckpointKind::FileEdit
            };
            let tool_use_id = request.metadata.get("tool_use_id");
            let mut record = templates
                .iter()
                .find(|record| {
                    record.kind == kind
                        && record.phase == phase
                        && tool_use_id.is_none_or(|id| record.tool_use_id.as_ref() == Some(id))
                })?
                .clone();
            record.timestamp_ns = unix_time_ns();
            record.trace_id.clone_from(&request.trace_id);
            if let Some(repo_work_dir) = request.files.first().map(|file| &file.repo_work_dir) {
                record.cwd.clone_from(repo_work_dir);
            }
            record.file_paths = request.files.iter().map(|file| file.path.clone()).collect();
            Some(record)
        })
        .collect()
}

impl SandboxedCheckpointRecord {
    fn from_event(preset: &str, event: ParsedHookEvent) -> Option<Self> {
        let timestamp_ns = unix_time_ns();
        let (kind, phase, context, tool_use_id, file_paths, parent_session_id) = match event {
            ParsedHookEvent::PreFileEdit(event) => (
                SandboxedCheckpointKind::FileEdit,
                SandboxedCheckpointPhase::Pre,
                event.context,
                event.tool_use_id,
                event.file_paths,
                None,
            ),
            ParsedHookEvent::PostFileEdit(event) => {
                let parent = event
                    .stream_source
                    .and_then(|source| source.external_parent_session_id);
                (
                    SandboxedCheckpointKind::FileEdit,
                    SandboxedCheckpointPhase::Post,
                    event.context,
                    event.tool_use_id,
                    event.file_paths,
                    parent,
                )
            }
            ParsedHookEvent::PreBashCall(event) => (
                SandboxedCheckpointKind::Bash,
                SandboxedCheckpointPhase::Pre,
                event.context,
                Some(event.tool_use_id),
                Vec::new(),
                None,
            ),
            ParsedHookEvent::PostBashCall(event) => {
                let parent = event
                    .stream_source
                    .and_then(|source| source.external_parent_session_id);
                (
                    SandboxedCheckpointKind::Bash,
                    SandboxedCheckpointPhase::Post,
                    event.context,
                    Some(event.tool_use_id),
                    Vec::new(),
                    parent,
                )
            }
            ParsedHookEvent::KnownHumanEdit(_) | ParsedHookEvent::UntrackedEdit(_) => return None,
        };

        Some(Self {
            version: FORMAT_VERSION,
            timestamp_ns,
            trace_id: context.trace_id,
            preset: preset.to_string(),
            kind,
            phase,
            agent_id: context.agent_id,
            external_session_id: context.external_session_id,
            external_parent_session_id: parent_session_id,
            tool_use_id,
            cwd: context.cwd,
            file_paths: file_paths.into_iter().take(1_000).collect(),
        })
    }
}

fn write_record(record: &SandboxedCheckpointRecord) -> Result<PathBuf, GitAiError> {
    let dir = sandboxed_checkpoint_dir();
    ensure_spool_dir(&dir)?;
    write_record_to_dir(record, &dir)
}

fn write_record_to_dir(
    record: &SandboxedCheckpointRecord,
    dir: &Path,
) -> Result<PathBuf, GitAiError> {
    let bytes = serde_json::to_vec(record)?;
    let pid = std::process::id();
    let mut temp_timestamp_ns = record.timestamp_ns;
    let (temp_path, mut file) = loop {
        let path = dir.join(format!(".{temp_timestamp_ns}.{pid}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                temp_timestamp_ns = temp_timestamp_ns.checked_add(1).ok_or_else(|| {
                    GitAiError::Generic("sandboxed checkpoint timestamp overflow".to_string())
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    };
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }
    drop(file);

    let mut final_timestamp_ns = record.timestamp_ns;
    loop {
        let final_path = dir.join(format!("{final_timestamp_ns}.ckpt"));
        match publish_record(&temp_path, &final_path) {
            Ok(()) => {
                if let Err(error) = fs::remove_file(&temp_path) {
                    tracing::debug!(path = %temp_path.display(), %error, "failed to clean sandboxed checkpoint temp file");
                }
                return Ok(final_path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let Some(next_timestamp_ns) = final_timestamp_ns.checked_add(1) else {
                    let _ = fs::remove_file(&temp_path);
                    return Err(GitAiError::Generic(
                        "sandboxed checkpoint timestamp overflow".to_string(),
                    ));
                };
                final_timestamp_ns = next_timestamp_ns;
            }
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                return Err(error.into());
            }
        }
    }
}

fn publish_record(temp_path: &Path, final_path: &Path) -> std::io::Result<()> {
    fs::hard_link(temp_path, final_path)
}

fn ensure_spool_dir(dir: &Path) -> Result<(), GitAiError> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Err(error) = fs::create_dir(dir)
                && error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error.into());
            }
        }
        Err(error) => return Err(error.into()),
    }

    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(GitAiError::Generic(format!(
            "refusing to use insecure sandboxed checkpoint path {}; expected a secure directory owned by the current user",
            dir.display()
        )));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn unix_time_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn sandboxed_checkpoint_dir() -> PathBuf {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(path) = std::env::var_os("GIT_AI_TEST_SANDBOX_CHECKPOINT_DIR") {
        return PathBuf::from(path);
    }

    PathBuf::from(SANDBOXED_CHECKPOINT_DIR)
}

fn poll_interval() -> Duration {
    #[cfg(any(test, feature = "test-support"))]
    if let Ok(raw) = std::env::var("GIT_AI_TEST_SANDBOX_CHECKPOINT_POLL_MS")
        && let Ok(milliseconds) = raw.parse::<u64>()
    {
        return Duration::from_millis(milliseconds);
    }

    POLL_INTERVAL
}

pub(crate) fn spawn_worker(
    telemetry: crate::daemon::telemetry_worker::DaemonTelemetryWorkerHandle,
) {
    tokio::spawn(async move {
        loop {
            let telemetry = telemetry.clone();
            match tokio::task::spawn_blocking(move || process_batch(&telemetry)).await {
                Ok(Ok(processed)) if processed > 0 => {
                    tracing::debug!(processed, "processed sandboxed checkpoints");
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "failed to process sandboxed checkpoints");
                }
                Err(error) => {
                    tracing::warn!(%error, "sandboxed checkpoint worker panicked");
                }
            }
            tokio::time::sleep(poll_interval()).await;
        }
    });
}

fn process_batch(
    telemetry: &crate::daemon::telemetry_worker::DaemonTelemetryWorkerHandle,
) -> Result<usize, GitAiError> {
    process_batch_with(|events| telemetry.persist_metrics_blocking(events))
}

fn process_batch_with(
    persist: impl FnOnce(&[MetricEvent]) -> Result<Vec<i64>, GitAiError>,
) -> Result<usize, GitAiError> {
    let dir = sandboxed_checkpoint_dir();
    ensure_spool_dir(&dir)?;
    process_batch_in_dir(&dir, persist)
}

fn process_batch_in_dir(
    dir: &Path,
    persist: impl FnOnce(&[MetricEvent]) -> Result<Vec<i64>, GitAiError>,
) -> Result<usize, GitAiError> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(0);
    };
    // Bound the directory scan itself. `read_dir` order is unspecified, so this
    // intentionally prioritizes a bounded scan over globally sorting a backlog.
    let mut paths = Vec::new();
    for entry in entries.take(MAX_FILES_PER_BATCH).flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("ckpt") {
            paths.push(path);
        } else {
            cleanup_spool_artifact(&path);
        }
    }
    paths.sort();

    let mut valid_paths = Vec::new();
    let mut events = Vec::new();
    let mut repositories = HashMap::new();
    for path in paths {
        let record = match read_record(&path) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "invalid sandboxed checkpoint");
                quarantine_invalid(&path);
                continue;
            }
        };
        let repo_url = repositories
            .entry(record.cwd.clone())
            .or_insert_with(|| crate::repo_url::resolve_repo_url_from_path(&record.cwd));
        events.extend(metric_events(&record, repo_url.as_deref()));
        valid_paths.push(path);
    }

    if valid_paths.is_empty() {
        return Ok(0);
    }
    persist(&events)?;
    for path in &valid_paths {
        if let Err(error) = fs::remove_file(path) {
            tracing::warn!(path = %path.display(), %error, "failed to remove sandboxed checkpoint");
        }
    }
    Ok(valid_paths.len())
}

fn cleanup_spool_artifact(path: &Path) {
    let extension = path.extension().and_then(|value| value.to_str());
    let should_remove = extension == Some("invalid")
        || (extension == Some("tmp")
            && fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age >= STALE_TEMP_FILE_AGE));
    if should_remove && let Err(error) = fs::remove_file(path) {
        tracing::debug!(path = %path.display(), %error, "failed to clean sandboxed checkpoint artifact");
    }
}

fn read_record(path: &Path) -> Result<SandboxedCheckpointRecord, GitAiError> {
    let bytes = fs::read(path)?;
    let record: SandboxedCheckpointRecord = serde_json::from_slice(&bytes)?;
    if record.version != FORMAT_VERSION {
        return Err(GitAiError::Generic(format!(
            "unsupported sandboxed checkpoint version {}",
            record.version
        )));
    }
    Ok(record)
}

fn quarantine_invalid(path: &Path) {
    let invalid = path.with_extension("invalid");
    if let Err(error) = fs::rename(path, &invalid) {
        tracing::warn!(path = %path.display(), %error, "failed to quarantine sandboxed checkpoint");
    }
}

fn metric_events(record: &SandboxedCheckpointRecord, repo_url: Option<&str>) -> Vec<MetricEvent> {
    let timestamp_secs = (record.timestamp_ns / 1_000_000_000).min(u32::MAX as u128) as u32;
    let recovery_metadata = serde_json::json!({
        "source_timestamp_ns": record.timestamp_ns.to_string(),
        "phase": record.phase.as_str(),
        "cwd": record.cwd.to_string_lossy(),
    })
    .to_string();

    let paths = if record.file_paths.is_empty() {
        vec![None]
    } else {
        record.file_paths.iter().map(Some).collect()
    };
    paths
        .into_iter()
        .map(|path| {
            let mut values = CheckpointValues::new()
                .checkpoint_ts(timestamp_secs.into())
                .kind("ai_agent")
                .lines_added(0)
                .lines_deleted(0)
                .lines_added_sloc(0)
                .lines_deleted_sloc(0)
                .edit_kind(record.kind.edit_kind())
                .checkpoint_type("sandboxed_fallback")
                .attribution_recovery_metadata(&recovery_metadata);
            if let Some(path) = path {
                values =
                    values.file_path(crate::utils::normalize_to_posix(&path.to_string_lossy()));
            }
            if let Some(tool_use_id) = record.tool_use_id.as_deref() {
                values = values.external_tool_use_id(tool_use_id);
            }

            let session_id =
                generate_session_id(&record.external_session_id, &record.agent_id.tool);
            let mut attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
                .session_id(session_id)
                .trace_id(&record.trace_id)
                .tool(&record.agent_id.tool)
                .model(&record.agent_id.model)
                .external_session_id(&record.external_session_id)
                .external_parent_session_id_opt(record.external_parent_session_id.clone());
            if let Some(repo_url) = repo_url {
                attrs = attrs.repo_url(repo_url);
            }
            MetricEvent::from_values_with_timestamp(values, attrs.to_sparse(), Some(timestamp_secs))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn record(timestamp_ns: u128) -> SandboxedCheckpointRecord {
        SandboxedCheckpointRecord {
            version: FORMAT_VERSION,
            timestamp_ns,
            trace_id: format!("trace-{timestamp_ns}"),
            preset: "codex".to_string(),
            kind: SandboxedCheckpointKind::FileEdit,
            phase: SandboxedCheckpointPhase::Post,
            agent_id: AgentId {
                tool: "codex".to_string(),
                id: "session".to_string(),
                model: "gpt-5".to_string(),
            },
            external_session_id: "session".to_string(),
            external_parent_session_id: None,
            tool_use_id: Some("tool-use".to_string()),
            cwd: PathBuf::from("/no/repository"),
            file_paths: vec![PathBuf::from("/no/repository/src/lib.rs")],
        }
    }

    #[test]
    fn batch_is_limited_to_one_thousand_files() {
        let dir = tempfile::tempdir().unwrap();
        for timestamp in 1..=1_001 {
            let path = dir.path().join(format!("{timestamp}.ckpt"));
            fs::write(path, serde_json::to_vec(&record(timestamp)).unwrap()).unwrap();
        }
        let event_count = AtomicUsize::new(0);
        let processed = process_batch_in_dir(dir.path(), |events| {
            event_count.store(events.len(), Ordering::SeqCst);
            Ok(Vec::new())
        })
        .unwrap();

        assert_eq!(processed, 1_000);
        assert_eq!(event_count.load(Ordering::SeqCst), 1_000);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_persistence_retains_valid_files_and_quarantines_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("1.ckpt"),
            serde_json::to_vec(&record(1)).unwrap(),
        )
        .unwrap();
        fs::write(dir.path().join("2.ckpt"), b"not json").unwrap();

        let result = process_batch_in_dir(dir.path(), |_| {
            Err(GitAiError::Generic("db unavailable".into()))
        });

        assert!(result.is_err());
        assert!(dir.path().join("1.ckpt").exists());
        assert!(dir.path().join("2.invalid").exists());
    }

    #[test]
    fn junk_files_do_not_permanently_block_checkpoint_processing() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..MAX_FILES_PER_BATCH {
            fs::write(dir.path().join(format!("{index}.invalid")), b"invalid").unwrap();
        }
        fs::write(
            dir.path().join("checkpoint.ckpt"),
            serde_json::to_vec(&record(1)).unwrap(),
        )
        .unwrap();

        let event_count = AtomicUsize::new(0);
        let mut processed = 0;
        for _ in 0..2 {
            processed += process_batch_in_dir(dir.path(), |events| {
                event_count.fetch_add(events.len(), Ordering::SeqCst);
                Ok(Vec::new())
            })
            .unwrap();
        }

        assert_eq!(processed, 1);
        assert_eq!(event_count.load(Ordering::SeqCst), 1);
        assert!(
            fs::read_dir(dir.path()).unwrap().flatten().all(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) != Some("invalid")
            }),
            "quarantined files should be cleaned within the bounded scans"
        );
    }

    #[test]
    fn request_fallback_only_captures_the_unsent_requests() {
        let input = json!({
            "session_id": "session",
            "cwd": "/tmp",
            "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch",
            "tool_use_id": "tool-use",
            "tool_input": {
                "patch": "*** Update File: generated.txt\n"
            }
        })
        .to_string();
        let templates = records_to_capture("codex", &input).unwrap();
        let agent_id = templates[0].agent_id.clone();
        let request = |repo: &str| {
            use crate::commands::checkpoint_agent::orchestrator::{
                BaseCommit, CheckpointFile, CheckpointRequest,
            };
            use crate::daemon::checkpoint::PreparedPathRole;

            CheckpointRequest {
                trace_id: "trace".to_string(),
                checkpoint_kind: crate::authorship::working_log::CheckpointKind::AiAgent,
                agent_id: Some(agent_id.clone()),
                files: vec![CheckpointFile {
                    path: PathBuf::from(repo).join("generated.txt"),
                    content: Some("generated".to_string()),
                    repo_work_dir: PathBuf::from(repo),
                    base_commit: BaseCommit::Initial,
                }],
                path_role: PreparedPathRole::Edited,
                stream_source: None,
                metadata: HashMap::from([("edit_kind".to_string(), "file_edit".to_string())]),
            }
        };
        let requests = [request("/repo/already-sent"), request("/repo/unsent")];

        let records = records_for_requests(&templates, &requests[1..]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cwd, PathBuf::from("/repo/unsent"));
        assert_eq!(
            records[0].file_paths,
            vec![PathBuf::from("/repo/unsent/generated.txt")]
        );
    }

    #[test]
    fn spool_directory_must_not_be_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let link = root.path().join("spool");
        fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        let error = ensure_spool_dir(&link).unwrap_err();
        assert!(error.to_string().contains("secure directory"));
    }

    #[test]
    fn same_timestamp_records_do_not_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let first = record(1);
        let mut second = record(1);
        second.trace_id = "second-record".to_string();

        let first_path = write_record_to_dir(&first, dir.path()).unwrap();
        let second_path = write_record_to_dir(&second, dir.path()).unwrap();

        assert_ne!(first_path, second_path);
        assert_eq!(read_record(&first_path).unwrap().trace_id, first.trace_id);
        assert_eq!(read_record(&second_path).unwrap().trace_id, second.trace_id);
    }

    #[test]
    fn publishing_a_record_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("record.tmp");
        let final_path = dir.path().join("record.ckpt");
        fs::write(&temp_path, b"new").unwrap();
        fs::write(&final_path, b"existing").unwrap();

        let error = publish_record(&temp_path, &final_path).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&final_path).unwrap(), b"existing");
    }
}
