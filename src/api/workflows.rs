//! Custom workflow API endpoints.
//!
//! These helpers intentionally mirror the server's portable workflow API, not
//! the underlying hosted/self-hosted backends.

use crate::api::client::ApiClient;
use crate::api::types::ApiErrorResponse;
use crate::error::GitAiError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowUploadRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activate: Option<bool>,
    pub definition: WorkflowUploadDefinition,
    pub deployment: WorkflowUploadDeployment,
    pub bundle: WorkflowUploadBundle,
    pub triggers: Vec<WorkflowUploadTrigger>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowUploadDefinition {
    pub slug: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowUploadDeployment {
    pub version: String,
    pub runtime: String,
    pub backend: String,
    pub bundle_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    pub manifest_json: serde_json::Value,
    #[serde(default)]
    pub permissions_json: serde_json::Value,
    #[serde(default)]
    pub limits_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowUploadBundle {
    pub storage_backend: String,
    pub object_key: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<WorkflowUploadBundleSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowUploadBundleSignature {
    pub key_id: String,
    pub algorithm: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowUploadTrigger {
    #[serde(rename = "type")]
    pub trigger_type: String,
    #[serde(default)]
    pub filter: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowUploadResponse {
    #[serde(default)]
    pub organization_id: Option<String>,
    pub workflow_definition_id: String,
    pub workflow_deployment_id: String,
    pub workflow_bundle_id: String,
    pub workflow_trigger_ids: Vec<String>,
    #[serde(default)]
    pub workflow_definition_status: Option<String>,
    #[serde(default)]
    pub workflow_deployment_status: Option<String>,
    pub activated: bool,
    #[serde(default)]
    pub review_required: bool,
    #[serde(default)]
    pub review_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDeploymentControlResponse {
    pub workflow_definition_id: String,
    pub workflow_deployment_id: String,
    pub status: String,
    #[serde(default)]
    pub rolled_back_from_deployment_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinitionArchiveResponse {
    pub workflow_definition_id: String,
    pub status: String,
    pub archived_deployments: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinitionRestoreResponse {
    pub workflow_definition_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDeploymentRuntimeKeyResponse {
    pub workflow_definition_id: String,
    pub workflow_deployment_id: String,
    #[serde(default)]
    pub key: Option<WorkflowDeploymentRuntimeKeySummary>,
    pub revoked: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDeploymentRuntimeKeySummary {
    pub id: String,
    pub key_hash: String,
    #[serde(default)]
    pub permissions: serde_json::Value,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPrSynchronizeTriggerRequest {
    pub event: serde_json::Value,
    pub unique: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key_suffix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTriggerResponse {
    pub accepted: bool,
    pub event_id: String,
    pub event_type: String,
    pub organization_id: String,
    pub idempotency_key: String,
    pub unique: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPrSynchronizeBackfillRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repositories: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_numbers: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key_suffix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPrSynchronizeBackfillEvent {
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub repository: String,
    pub pull_number: u32,
    pub latest_sync_seq: u32,
    #[serde(default)]
    pub occurred_at: Option<String>,
    pub dry_run: bool,
    pub enqueued: bool,
    #[serde(default)]
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPrSynchronizeBackfillResponse {
    pub accepted: bool,
    pub dry_run: bool,
    pub scanned: u32,
    pub matched: u32,
    pub enqueued: u32,
    pub skipped: u32,
    #[serde(default)]
    pub events: Vec<WorkflowPrSynchronizeBackfillEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCapabilitiesResponse {
    pub sdk: WorkflowSdkCompatibilityPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSdkCompatibilityPolicy {
    pub sdk_package: String,
    pub supported_versions: Vec<String>,
    pub version_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinitionsResponse {
    pub workflows: Vec<WorkflowDefinitionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinitionSummary {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub current_deployment_id: Option<String>,
    pub current_deployment: Option<WorkflowDeploymentSummary>,
    #[serde(default)]
    pub triggers: Vec<WorkflowTriggerSummary>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDeploymentSummary {
    pub id: String,
    pub version: String,
    pub runtime: String,
    pub backend: String,
    pub status: String,
    pub bundle_digest: String,
    pub source_digest: Option<String>,
    pub activated_at: Option<String>,
    pub disabled_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTriggerSummary {
    pub id: String,
    pub trigger_type: String,
    pub enabled: bool,
    #[serde(default)]
    pub filter: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunsResponse {
    pub runs: Vec<WorkflowRunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunSummary {
    pub id: String,
    pub workflow_definition_id: String,
    pub deployment_id: String,
    pub trigger_type: String,
    pub trigger_idempotency_key: String,
    pub status: String,
    pub backend: String,
    pub backend_instance_id: Option<String>,
    pub attempt: u32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub definition: Option<WorkflowRunDefinitionRef>,
    pub deployment: Option<WorkflowRunDeploymentRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunDefinitionRef {
    pub id: String,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunDeploymentRef {
    pub id: String,
    pub version: String,
    pub backend: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunDetail {
    #[serde(flatten)]
    pub summary: WorkflowRunSummary,
    #[serde(default)]
    pub event_payload: serde_json::Value,
    #[serde(default)]
    pub output: serde_json::Value,
    #[serde(default)]
    pub error: serde_json::Value,
    #[serde(default)]
    pub steps: Vec<WorkflowStepSummary>,
    #[serde(default)]
    pub waits: Vec<WorkflowWaitSummary>,
    #[serde(default)]
    pub artifacts: Vec<WorkflowArtifactSummary>,
    #[serde(default)]
    pub token_leases: Vec<WorkflowRunTokenLeaseSummary>,
    #[serde(default)]
    pub recent_logs: Vec<WorkflowLogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepSummary {
    pub id: String,
    pub step_key: String,
    pub step_name: String,
    pub step_type: String,
    pub status: String,
    pub attempt: u32,
    pub output_artifact_id: Option<String>,
    #[serde(default)]
    pub error: serde_json::Value,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowWaitSummary {
    pub id: String,
    pub step_id: Option<String>,
    pub wait_type: String,
    pub event_type: Option<String>,
    pub wake_at: Option<String>,
    pub timeout_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLogEntry {
    pub id: String,
    pub run_id: String,
    pub step_id: Option<String>,
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub fields: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArtifactSummary {
    pub id: String,
    pub run_id: String,
    pub step_id: Option<String>,
    pub storage_backend: String,
    pub object_key: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub digest: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunTokenLeaseSummary {
    pub id: String,
    pub run_id: String,
    pub step_id: Option<String>,
    pub provider: String,
    pub scm_connection_id: String,
    pub repo_id: Option<String>,
    #[serde(default)]
    pub requested_permissions: Vec<String>,
    pub expires_at: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowLogsResponse {
    pub run: WorkflowRunSummary,
    pub logs: Vec<WorkflowLogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSecretsResponse {
    pub secrets: Vec<WorkflowSecretSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSecretSummary {
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSecretSetRequest {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSecretSetResponse {
    pub secret: WorkflowSecretSummary,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSecretDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNotificationRoutesResponse {
    pub routes: Vec<WorkflowNotificationRouteSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowNotificationRouteSummary {
    pub id: String,
    pub channel: String,
    pub transport: String,
    pub target_host: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowNotificationRouteSetRequest {
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNotificationRouteSetResponse {
    pub route: WorkflowNotificationRouteSummary,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNotificationRouteDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunControlResponse {
    pub accepted: bool,
    pub action: String,
    pub run_id: String,
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunRefreshResponse {
    pub run_id: String,
    pub backend: String,
    pub backend_instance_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunRestartRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_step: Option<String>,
}

impl ApiClient {
    pub fn get_workflow_capabilities(&self) -> Result<WorkflowCapabilitiesResponse, GitAiError> {
        let response = self.context().get("/api/workflows/capabilities")?;
        parse_json_response(response, "Workflow capabilities", &[200])
    }

    pub fn upload_workflow(
        &self,
        request: &WorkflowUploadRequest,
    ) -> Result<WorkflowUploadResponse, GitAiError> {
        let response = self.context().post_json("/api/workflows/upload", request)?;
        parse_json_response(response, "Workflow upload", &[201])
    }

    pub fn list_workflows(
        &self,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<WorkflowDefinitionsResponse, GitAiError> {
        let endpoint = workflow_query_endpoint(
            "/api/workflows",
            &[
                ("status", status.map(str::to_string)),
                ("limit", limit.map(|value| value.to_string())),
            ],
        );
        let response = self.context().get(&endpoint)?;
        parse_json_response(response, "Workflow list", &[200])
    }

    pub fn archive_workflow_definition(
        &self,
        workflow_definition_id: &str,
    ) -> Result<WorkflowDefinitionArchiveResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/archive",
            encode_query_component(workflow_definition_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow definition archive", &[200])
    }

    pub fn restore_workflow_definition(
        &self,
        workflow_definition_id: &str,
    ) -> Result<WorkflowDefinitionRestoreResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/restore",
            encode_query_component(workflow_definition_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow definition restore", &[200])
    }

    pub fn activate_workflow_deployment(
        &self,
        workflow_definition_id: &str,
        workflow_deployment_id: &str,
    ) -> Result<WorkflowDeploymentControlResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/deployments/{}/activate",
            encode_query_component(workflow_definition_id),
            encode_query_component(workflow_deployment_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow deployment activate", &[200])
    }

    pub fn approve_workflow_deployment_review(
        &self,
        workflow_definition_id: &str,
        workflow_deployment_id: &str,
    ) -> Result<WorkflowDeploymentControlResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/deployments/{}/approve",
            encode_query_component(workflow_definition_id),
            encode_query_component(workflow_deployment_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow deployment approve", &[200])
    }

    pub fn reject_workflow_deployment_review(
        &self,
        workflow_definition_id: &str,
        workflow_deployment_id: &str,
    ) -> Result<WorkflowDeploymentControlResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/deployments/{}/reject",
            encode_query_component(workflow_definition_id),
            encode_query_component(workflow_deployment_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow deployment reject", &[200])
    }

    pub fn disable_workflow_deployment(
        &self,
        workflow_definition_id: &str,
        workflow_deployment_id: &str,
    ) -> Result<WorkflowDeploymentControlResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/deployments/{}/disable",
            encode_query_component(workflow_definition_id),
            encode_query_component(workflow_deployment_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow deployment disable", &[200])
    }

    pub fn rollback_workflow_deployment(
        &self,
        workflow_definition_id: &str,
        workflow_deployment_id: &str,
    ) -> Result<WorkflowDeploymentControlResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/deployments/{}/rollback",
            encode_query_component(workflow_definition_id),
            encode_query_component(workflow_deployment_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow deployment rollback", &[200])
    }

    pub fn rotate_workflow_deployment_runtime_key(
        &self,
        workflow_definition_id: &str,
        workflow_deployment_id: &str,
    ) -> Result<WorkflowDeploymentRuntimeKeyResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/deployments/{}/runtime-key/rotate",
            encode_query_component(workflow_definition_id),
            encode_query_component(workflow_deployment_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow deployment runtime key rotate", &[200])
    }

    pub fn revoke_workflow_deployment_runtime_keys(
        &self,
        workflow_definition_id: &str,
        workflow_deployment_id: &str,
    ) -> Result<WorkflowDeploymentRuntimeKeyResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/definitions/{}/deployments/{}/runtime-key/revoke",
            encode_query_component(workflow_definition_id),
            encode_query_component(workflow_deployment_id),
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow deployment runtime key revoke", &[200])
    }

    pub fn trigger_pr_synchronize_workflow(
        &self,
        event: serde_json::Value,
        unique: bool,
        idempotency_key_suffix: Option<String>,
    ) -> Result<WorkflowTriggerResponse, GitAiError> {
        let request = WorkflowPrSynchronizeTriggerRequest {
            event,
            unique,
            idempotency_key_suffix,
        };
        let response = self
            .context()
            .post_json("/api/workflows/triggers/pr.synchronize", &request)?;
        parse_json_response(response, "Workflow pr.synchronize trigger", &[202])
    }

    pub fn backfill_pr_synchronize_workflow(
        &self,
        request: &WorkflowPrSynchronizeBackfillRequest,
    ) -> Result<WorkflowPrSynchronizeBackfillResponse, GitAiError> {
        let response = self
            .context()
            .post_json("/api/workflows/triggers/pr.synchronize/backfill", request)?;
        parse_json_response(response, "Workflow pr.synchronize backfill", &[202])
    }

    pub fn list_workflow_runs(
        &self,
        workflow_definition_id: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<WorkflowRunsResponse, GitAiError> {
        let endpoint = workflow_query_endpoint(
            "/api/workflows/runs",
            &[
                (
                    "workflowDefinitionId",
                    workflow_definition_id.map(str::to_string),
                ),
                ("status", status.map(str::to_string)),
                ("limit", limit.map(|value| value.to_string())),
            ],
        );
        let response = self.context().get(&endpoint)?;
        parse_json_response(response, "Workflow runs", &[200])
    }

    pub fn get_workflow_run(&self, run_id: &str) -> Result<WorkflowRunDetail, GitAiError> {
        let endpoint = format!("/api/workflows/runs/{}", encode_query_component(run_id));
        let response = self.context().get(&endpoint)?;
        parse_json_response(response, "Workflow run", &[200])
    }

    pub fn get_workflow_artifact(
        &self,
        run_id: &str,
        artifact_id: &str,
    ) -> Result<serde_json::Value, GitAiError> {
        let endpoint = format!(
            "/api/workflows/runs/{}/artifacts/{}",
            encode_query_component(run_id),
            encode_query_component(artifact_id),
        );
        let response = self.context().get(&endpoint)?;
        parse_json_response(response, "Workflow artifact", &[200])
    }

    pub fn list_workflow_logs(
        &self,
        run_id: &str,
        level: Option<&str>,
        limit: Option<u32>,
    ) -> Result<WorkflowLogsResponse, GitAiError> {
        let base = format!(
            "/api/workflows/runs/{}/logs",
            encode_query_component(run_id)
        );
        let endpoint = workflow_query_endpoint(
            &base,
            &[
                ("level", level.map(str::to_string)),
                ("limit", limit.map(|value| value.to_string())),
            ],
        );
        let response = self.context().get(&endpoint)?;
        parse_json_response(response, "Workflow logs", &[200])
    }

    pub fn cancel_workflow_run(
        &self,
        run_id: &str,
    ) -> Result<WorkflowRunControlResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/runs/{}/cancel",
            encode_query_component(run_id)
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow run cancel", &[202])
    }

    pub fn restart_workflow_run(
        &self,
        run_id: &str,
        from_step: Option<String>,
    ) -> Result<WorkflowRunControlResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/runs/{}/restart",
            encode_query_component(run_id)
        );
        let request = WorkflowRunRestartRequest { from_step };
        let response = self.context().post_json(&endpoint, &request)?;
        parse_json_response(response, "Workflow run restart", &[202])
    }

    pub fn refresh_workflow_run(
        &self,
        run_id: &str,
    ) -> Result<WorkflowRunRefreshResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/runs/{}/refresh",
            encode_query_component(run_id)
        );
        let response = self
            .context()
            .post_json(&endpoint, &serde_json::json!({}))?;
        parse_json_response(response, "Workflow run refresh", &[200])
    }

    pub fn list_workflow_secrets(&self) -> Result<WorkflowSecretsResponse, GitAiError> {
        let response = self.context().get("/api/workflows/secrets")?;
        parse_json_response(response, "Workflow secrets list", &[200])
    }

    pub fn set_workflow_secret(
        &self,
        name: &str,
        value: &str,
    ) -> Result<WorkflowSecretSetResponse, GitAiError> {
        let request = WorkflowSecretSetRequest {
            name: name.to_string(),
            value: value.to_string(),
        };
        let response = self
            .context()
            .post_json("/api/workflows/secrets", &request)?;
        parse_json_response(response, "Workflow secret set", &[200, 201])
    }

    pub fn delete_workflow_secret(
        &self,
        name: &str,
    ) -> Result<WorkflowSecretDeleteResponse, GitAiError> {
        let endpoint = format!("/api/workflows/secrets/{}", encode_query_component(name));
        let response = self.context().delete(&endpoint)?;
        parse_json_response(response, "Workflow secret delete", &[200])
    }

    pub fn list_workflow_notification_routes(
        &self,
    ) -> Result<WorkflowNotificationRoutesResponse, GitAiError> {
        let response = self.context().get("/api/workflows/notification-routes")?;
        parse_json_response(response, "Workflow notification routes list", &[200])
    }

    pub fn set_workflow_notification_route(
        &self,
        channel: &str,
        transport: Option<&str>,
        target_url: Option<&str>,
        enabled: bool,
    ) -> Result<WorkflowNotificationRouteSetResponse, GitAiError> {
        let request = WorkflowNotificationRouteSetRequest {
            channel: channel.to_string(),
            transport: transport.map(str::to_string),
            target_url: target_url.map(str::to_string),
            enabled,
        };
        let response = self
            .context()
            .post_json("/api/workflows/notification-routes", &request)?;
        parse_json_response(response, "Workflow notification route set", &[200, 201])
    }

    pub fn delete_workflow_notification_route(
        &self,
        channel: &str,
    ) -> Result<WorkflowNotificationRouteDeleteResponse, GitAiError> {
        let endpoint = format!(
            "/api/workflows/notification-routes/{}",
            encode_query_component(channel)
        );
        let response = self.context().delete(&endpoint)?;
        parse_json_response(response, "Workflow notification route delete", &[200])
    }
}

fn parse_json_response<T: for<'de> Deserialize<'de>>(
    response: crate::http::Response,
    operation: &str,
    success_statuses: &[u16],
) -> Result<T, GitAiError> {
    let status_code = response.status_code;
    let body = response
        .as_str()
        .map_err(|e| GitAiError::Generic(format!("Failed to read response body: {}", e)))?;

    if success_statuses.contains(&status_code) {
        return serde_json::from_str(body).map_err(GitAiError::JsonError);
    }

    let error_response: ApiErrorResponse =
        serde_json::from_str(body).unwrap_or_else(|_| ApiErrorResponse {
            error: body.to_string(),
            details: None,
        });
    Err(GitAiError::Generic(format!(
        "{} failed with status {}: {}",
        operation, status_code, error_response.error
    )))
}

pub fn workflow_query_endpoint(base: &str, params: &[(&str, Option<String>)]) -> String {
    let pairs: Vec<String> = params
        .iter()
        .filter_map(|(key, value)| {
            value.as_ref().map(|value| {
                format!(
                    "{}={}",
                    encode_query_component(key),
                    encode_query_component(value)
                )
            })
        })
        .collect();

    if pairs.is_empty() {
        base.to_string()
    } else {
        format!("{}?{}", base, pairs.join("&"))
    }
}

fn encode_query_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_query_endpoint_omits_empty_params() {
        assert_eq!(
            workflow_query_endpoint(
                "/api/workflows/runs",
                &[
                    ("workflowDefinitionId", Some("workflow_def_1".to_string())),
                    ("status", None),
                    ("limit", Some("50".to_string())),
                ],
            ),
            "/api/workflows/runs?workflowDefinitionId=workflow_def_1&limit=50"
        );
    }

    #[test]
    fn workflow_query_endpoint_encodes_params() {
        assert_eq!(
            workflow_query_endpoint(
                "/api/workflows/runs",
                &[("workflowDefinitionId", Some("workflow def/1".to_string()))],
            ),
            "/api/workflows/runs?workflowDefinitionId=workflow+def%2F1"
        );
    }

    #[test]
    fn workflow_control_endpoint_parts_encode_run_ids() {
        let run_id = "workflow run/1";
        assert_eq!(encode_query_component(run_id), "workflow+run%2F1");
        assert_eq!(
            format!(
                "/api/workflows/runs/{}/cancel",
                encode_query_component(run_id)
            ),
            "/api/workflows/runs/workflow+run%2F1/cancel"
        );
        assert_eq!(
            format!(
                "/api/workflows/runs/{}/refresh",
                encode_query_component(run_id)
            ),
            "/api/workflows/runs/workflow+run%2F1/refresh"
        );
        assert_eq!(
            format!(
                "/api/workflows/runs/{}/artifacts/{}",
                encode_query_component(run_id),
                encode_query_component("artifact/1")
            ),
            "/api/workflows/runs/workflow+run%2F1/artifacts/artifact%2F1"
        );
    }

    #[test]
    fn workflow_notification_route_endpoint_parts_encode_channels() {
        assert_eq!(
            format!(
                "/api/workflows/notification-routes/{}",
                encode_query_component("team/alerts")
            ),
            "/api/workflows/notification-routes/team%2Falerts"
        );
    }

    #[test]
    fn workflow_deployment_control_endpoint_parts_encode_ids() {
        let workflow_definition_id = "workflow def/1";
        let workflow_deployment_id = "workflow dep/1";
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/archive",
                encode_query_component(workflow_definition_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/archive"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/restore",
                encode_query_component(workflow_definition_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/restore"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/deployments/{}/activate",
                encode_query_component(workflow_definition_id),
                encode_query_component(workflow_deployment_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/deployments/workflow+dep%2F1/activate"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/deployments/{}/approve",
                encode_query_component(workflow_definition_id),
                encode_query_component(workflow_deployment_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/deployments/workflow+dep%2F1/approve"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/deployments/{}/reject",
                encode_query_component(workflow_definition_id),
                encode_query_component(workflow_deployment_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/deployments/workflow+dep%2F1/reject"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/deployments/{}/disable",
                encode_query_component(workflow_definition_id),
                encode_query_component(workflow_deployment_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/deployments/workflow+dep%2F1/disable"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/deployments/{}/rollback",
                encode_query_component(workflow_definition_id),
                encode_query_component(workflow_deployment_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/deployments/workflow+dep%2F1/rollback"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/deployments/{}/runtime-key/rotate",
                encode_query_component(workflow_definition_id),
                encode_query_component(workflow_deployment_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/deployments/workflow+dep%2F1/runtime-key/rotate"
        );
        assert_eq!(
            format!(
                "/api/workflows/definitions/{}/deployments/{}/runtime-key/revoke",
                encode_query_component(workflow_definition_id),
                encode_query_component(workflow_deployment_id),
            ),
            "/api/workflows/definitions/workflow+def%2F1/deployments/workflow+dep%2F1/runtime-key/revoke"
        );
    }

    #[test]
    fn workflow_restart_request_omits_empty_from_step() {
        let request = WorkflowRunRestartRequest { from_step: None };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({})
        );

        let request = WorkflowRunRestartRequest {
            from_step: Some("aggregate".to_string()),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({ "fromStep": "aggregate" })
        );
    }

    #[test]
    fn workflow_trigger_request_uses_camel_case_options() {
        let request = WorkflowPrSynchronizeTriggerRequest {
            event: serde_json::json!({ "type": "pr.synchronize" }),
            unique: true,
            idempotency_key_suffix: Some("dev-1".to_string()),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "event": { "type": "pr.synchronize" },
                "unique": true,
                "idempotencyKeySuffix": "dev-1",
            })
        );
    }

    #[test]
    fn workflow_capabilities_response_parses_sdk_policy() {
        let response: WorkflowCapabilitiesResponse = serde_json::from_value(serde_json::json!({
            "sdk": {
                "sdkPackage": "@git-ai-project/workflows",
                "supportedVersions": ["0.0.0"],
                "versionPolicy": "exact",
            },
        }))
        .unwrap();

        assert_eq!(response.sdk.sdk_package, "@git-ai-project/workflows");
        assert_eq!(response.sdk.supported_versions, vec!["0.0.0"]);
        assert_eq!(response.sdk.version_policy, "exact");
    }

    #[test]
    fn workflow_runtime_key_response_parses_key_metadata() {
        let response: WorkflowDeploymentRuntimeKeyResponse =
            serde_json::from_value(serde_json::json!({
                "workflowDefinitionId": "workflow_def_1",
                "workflowDeploymentId": "workflow_dep_1",
                "key": {
                    "id": "workflow_runtime_api_key_1",
                    "keyHash": "sha256:key",
                    "permissions": {
                        "workflow.token": ["lease"]
                    },
                    "expiresAt": null,
                    "revokedAt": null,
                    "createdAt": "2026-06-05T16:00:00.000Z"
                },
                "revoked": 1
            }))
            .unwrap();

        let key = response.key.unwrap();
        assert_eq!(response.workflow_definition_id, "workflow_def_1");
        assert_eq!(response.workflow_deployment_id, "workflow_dep_1");
        assert_eq!(key.id, "workflow_runtime_api_key_1");
        assert_eq!(key.key_hash, "sha256:key");
        assert_eq!(
            key.permissions["workflow.token"],
            serde_json::json!(["lease"])
        );
        assert_eq!(response.revoked, 1);
    }

    #[test]
    fn workflow_run_detail_parses_artifact_metadata() {
        let detail: WorkflowRunDetail = serde_json::from_value(serde_json::json!({
            "id": "workflow_run_1",
            "workflowDefinitionId": "workflow_def_1",
            "deploymentId": "workflow_dep_1",
            "triggerType": "pr.synchronize",
            "triggerIdempotencyKey": "pr.synchronize:org:repo:1:1",
            "status": "succeeded",
            "backend": "bullmq",
            "backendInstanceId": null,
            "attempt": 1,
            "startedAt": "2026-06-05T16:00:00.000Z",
            "completedAt": "2026-06-05T16:00:01.000Z",
            "createdAt": "2026-06-05T16:00:00.000Z",
            "updatedAt": "2026-06-05T16:00:01.000Z",
            "definition": {
                "id": "workflow_def_1",
                "slug": "risk",
                "name": "Risk"
            },
            "deployment": {
                "id": "workflow_dep_1",
                "version": "1.0.0",
                "backend": "bullmq",
                "status": "active"
            },
            "artifacts": [{
                "id": "workflow_artifact_1",
                "runId": "workflow_run_1",
                "stepId": "workflow_step_1",
                "storageBackend": "s3",
                "objectKey": "org/workflow_artifact_1.json",
                "contentType": "application/json",
                "sizeBytes": 42,
                "digest": "sha256:abc",
                "createdAt": "2026-06-05T16:00:01.000Z"
            }],
            "tokenLeases": [{
                "id": "workflow_token_lease_1",
                "runId": "workflow_run_1",
                "stepId": "workflow_step_1",
                "provider": "github",
                "scmConnectionId": "scm_connection_1",
                "repoId": "repo_1",
                "requestedPermissions": ["pull_requests.read"],
                "expiresAt": "2026-06-05T16:10:01.000Z",
                "createdAt": "2026-06-05T16:00:01.000Z"
            }]
        }))
        .unwrap();

        assert_eq!(detail.artifacts.len(), 1);
        assert_eq!(detail.artifacts[0].id, "workflow_artifact_1");
        assert_eq!(detail.artifacts[0].run_id, "workflow_run_1");
        assert_eq!(detail.artifacts[0].storage_backend, "s3");
        assert_eq!(detail.token_leases.len(), 1);
        assert_eq!(detail.token_leases[0].id, "workflow_token_lease_1");
        assert_eq!(detail.token_leases[0].provider, "github");
        assert_eq!(
            detail.token_leases[0].step_id.as_deref(),
            Some("workflow_step_1")
        );
        assert_eq!(
            detail.token_leases[0].requested_permissions,
            vec!["pull_requests.read"]
        );
    }

    #[test]
    fn workflow_upload_response_parses_lifecycle_review_metadata() {
        let response: WorkflowUploadResponse = serde_json::from_value(serde_json::json!({
            "organizationId": "org_1",
            "workflowDefinitionId": "workflow_def_1",
            "workflowDeploymentId": "workflow_dep_1",
            "workflowBundleId": "workflow_bundle_1",
            "workflowTriggerIds": ["workflow_trigger_1"],
            "workflowDefinitionStatus": "pending_review",
            "workflowDeploymentStatus": "pending_review",
            "activated": false,
            "reviewRequired": true,
            "reviewReasons": ["SCM write permissions"],
        }))
        .unwrap();

        assert_eq!(response.organization_id.as_deref(), Some("org_1"));
        assert_eq!(
            response.workflow_deployment_status.as_deref(),
            Some("pending_review")
        );
        assert!(response.review_required);
        assert_eq!(response.review_reasons, vec!["SCM write permissions"]);
    }

    #[test]
    fn workflow_upload_response_remains_compatible_with_legacy_shape() {
        let response: WorkflowUploadResponse = serde_json::from_value(serde_json::json!({
            "workflowDefinitionId": "workflow_def_1",
            "workflowDeploymentId": "workflow_dep_1",
            "workflowBundleId": "workflow_bundle_1",
            "workflowTriggerIds": ["workflow_trigger_1"],
            "activated": false,
        }))
        .unwrap();

        assert!(response.organization_id.is_none());
        assert!(response.workflow_deployment_status.is_none());
        assert!(!response.review_required);
        assert!(response.review_reasons.is_empty());
    }
}
