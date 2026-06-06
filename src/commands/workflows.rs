use crate::api::client::{ApiClient, ApiContext};
use crate::api::workflows::{
    WorkflowArtifactSummary, WorkflowLogEntry, WorkflowNotificationRouteSummary,
    WorkflowPrSynchronizeBackfillRequest, WorkflowPrSynchronizeBackfillResponse, WorkflowRunDetail,
    WorkflowRunTokenLeaseSummary, WorkflowUploadBundle, WorkflowUploadBundleSignature,
    WorkflowUploadDefinition, WorkflowUploadDeployment, WorkflowUploadRequest,
    WorkflowUploadResponse, WorkflowUploadTrigger,
};
use crate::error::GitAiError;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

const DEFAULT_MANIFEST_PATH: &str = "gitai.workflow.json";
const WORKFLOW_DEV_WATCH_POLL_MS: u64 = 1_000;
const WORKFLOW_DEV_WATCH_EXTENSIONS: &[&str] = &["js", "json", "mjs", "ts", "tsx"];
const DEFAULT_WORKFLOW_BUNDLE_MAX_BYTES: u64 = 10 * 1024 * 1024;
const WORKFLOW_SDK_PACKAGE_NAME: &str = "@git-ai-project/workflows";
const WORKFLOW_SDK_VERSION: &str = "0.0.0";
const SUPPORTED_WORKFLOW_RUNTIMES: &[&str] = &["node18", "node20", "node22"];
const MAX_WORKFLOW_REDACTION_ENTRIES: usize = 50;
const MAX_WORKFLOW_REDACTION_LITERAL_LENGTH: usize = 4096;
const MAX_WORKFLOW_REDACTION_PATTERN_LENGTH: usize = 512;
const SUPPORTED_SCM_PERMISSIONS: &[&str] = &[
    "*",
    "contents.read",
    "contents.write",
    "pull_requests.read",
    "pull_requests.write",
    "issues.read",
    "issues.write",
    "statuses.write",
];
const SUPPORTED_GIT_AI_PERMISSIONS: &[&str] = &[
    "pr.read",
    "repo.read",
    "metrics.write",
    "notifications.write",
    "artifacts.read",
    "artifacts.write",
];
const SUPPORTED_LIMIT_KEYS: &[&str] = &[
    "timeoutMs",
    "stepTimeoutMs",
    "maxConcurrentRuns",
    "maxConcurrentRunsPerOrganization",
    "concurrencyRetryAfterMs",
    "maxStepOutputBytes",
    "redactionLiterals",
    "redactionPatterns",
    "redactionDetectorPacks",
];
const SUPPORTED_REDACTION_DETECTOR_PACKS: &[&str] = &["common-secrets"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalWorkflowManifest {
    pub schema_version: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub version: String,
    pub entrypoint: String,
    pub runtime: String,
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default = "default_sdk_package")]
    pub sdk_package: String,
    #[serde(default)]
    pub sdk_version: Option<String>,
    #[serde(default)]
    pub permissions: serde_json::Value,
    #[serde(default)]
    pub limits: serde_json::Value,
    #[serde(default)]
    pub triggers: Vec<WorkflowUploadTrigger>,
}

#[derive(Debug, Clone)]
pub struct BundleOutput {
    pub output_dir: PathBuf,
    pub bundle_path: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_json: serde_json::Value,
    pub source_digest: String,
    pub bundle_digest: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum BundleMode {
    NodeEsbuild,
    CopyEntrypoint,
}

pub fn handle_workflows(args: &[String]) {
    let result = match args.first().map(|s| s.as_str()).unwrap_or("help") {
        "init" => handle_init(&args[1..]),
        "validate" => handle_validate(&args[1..]),
        "bundle" => handle_bundle(&args[1..]),
        "upload" => handle_upload(&args[1..]),
        "list" => handle_list(&args[1..]),
        "activate" => handle_activate(&args[1..]),
        "approve" => handle_approve(&args[1..]),
        "reject" => handle_reject(&args[1..]),
        "disable" => handle_disable(&args[1..]),
        "rollback" => handle_rollback(&args[1..]),
        "archive" => handle_archive(&args[1..]),
        "restore" => handle_restore(&args[1..]),
        "runtime-key" => handle_runtime_key(&args[1..]),
        "runs" => handle_runs(&args[1..]),
        "inspect" => handle_inspect(&args[1..]),
        "logs" => handle_logs(&args[1..]),
        "artifacts" => handle_artifacts(&args[1..]),
        "trigger" => handle_trigger(&args[1..]),
        "backfill" => handle_backfill(&args[1..]),
        "cancel" => handle_cancel(&args[1..]),
        "refresh" => handle_refresh(&args[1..]),
        "restart" => handle_restart(&args[1..]),
        "secrets" => handle_secrets(&args[1..]),
        "notifications" => handle_notifications(&args[1..]),
        "dev" => handle_dev(&args[1..]),
        "help" | "--help" | "-h" => {
            print_workflows_help();
            Ok(())
        }
        other => Err(GitAiError::Generic(format!(
            "Unknown workflows subcommand '{}'",
            other
        ))),
    };

    if let Err(error) = result {
        eprintln!("Error: {}", error);
        std::process::exit(1);
    }
}

fn handle_init(args: &[String]) -> Result<(), GitAiError> {
    let mut name = "PR Risk Review".to_string();
    let mut dir = PathBuf::from(".");
    let mut i = 0;
    if let Some(first) = args.first()
        && !first.starts_with('-')
    {
        name = first.clone();
        i = 1;
    }
    while i < args.len() {
        match args[i].as_str() {
            "--dir" if i + 1 < args.len() => {
                dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--help" | "-h" => {
                print_init_help();
                return Ok(());
            }
            other => {
                return Err(GitAiError::Generic(format!(
                    "Unknown workflows init argument '{}'",
                    other
                )));
            }
        }
    }

    let slug = normalize_slug(&name);
    fs::create_dir_all(dir.join("fixtures"))?;
    write_new_file(dir.join("gitai.workflow.ts"), sample_workflow_ts(&name))?;
    write_new_file(
        dir.join(DEFAULT_MANIFEST_PATH),
        sample_manifest_json(&slug, &name)?,
    )?;
    write_new_file(
        dir.join("fixtures/pr.synchronize.json"),
        sample_fixture_json()?,
    )?;
    write_new_file(dir.join("package.json"), sample_package_json()?)?;
    write_new_file(dir.join("tsconfig.json"), sample_tsconfig_json()?)?;

    println!("Initialized workflow project in {}", dir.display());
    println!("Next: git-ai workflows validate");
    Ok(())
}

fn handle_validate(args: &[String]) -> Result<(), GitAiError> {
    let options = CommonOptions::parse(args)?;
    let (manifest, manifest_json, manifest_path) = read_manifest(options.manifest.as_deref())?;
    validate_manifest(&manifest, &manifest_json, &manifest_path)?;
    println!("Workflow '{}' ({}) is valid", manifest.name, manifest.slug);
    Ok(())
}

fn handle_bundle(args: &[String]) -> Result<(), GitAiError> {
    let options = BundleOptions::parse(args)?;
    let output = bundle_workflow(options.manifest.as_deref(), options.out_dir.as_deref())?;
    println!("Bundle: {}", output.bundle_path.display());
    println!("Manifest: {}", output.manifest_path.display());
    println!("Source digest: {}", output.source_digest);
    println!("Bundle digest: {}", output.bundle_digest);
    println!("Size: {} bytes", output.size_bytes);
    Ok(())
}

fn handle_dev(args: &[String]) -> Result<(), GitAiError> {
    let options = DevOptions::parse(args)?;
    run_workflow_dev(options)
}

fn handle_upload(args: &[String]) -> Result<(), GitAiError> {
    let options = UploadOptions::parse(args)?;
    let (manifest, manifest_json, manifest_path) = read_manifest(options.manifest.as_deref())?;
    validate_manifest(&manifest, &manifest_json, &manifest_path)?;
    let bundle_signature = read_bundle_signature(&options)?;
    let bundle = if let Some(bundle_path) = options.bundle_path {
        bundle_from_existing(
            &manifest,
            &manifest_json,
            parent_dir_or_current(&manifest_path),
            &bundle_path,
        )?
    } else {
        bundle_workflow(Some(&manifest_path), None)?
    };
    let bundle_bytes = fs::read(&bundle.bundle_path).map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to read workflow bundle '{}': {}",
            bundle.bundle_path.display(),
            e
        ))
    })?;

    let client = ApiClient::new(ApiContext::new(None));
    if !client.has_api_key() {
        return Err(GitAiError::Generic(
            "workflow upload requires config.api_key with workflow.definition.write".to_string(),
        ));
    }
    validate_manifest_sdk_with_server(&client, &manifest)?;

    let bundle_digest = bundle.bundle_digest.clone();
    let request = WorkflowUploadRequest {
        activate: Some(options.activate),
        definition: WorkflowUploadDefinition {
            slug: manifest.slug,
            name: manifest.name,
            description: manifest.description,
        },
        deployment: WorkflowUploadDeployment {
            version: manifest.version,
            runtime: manifest.runtime,
            backend: options.backend.unwrap_or(manifest.backend),
            bundle_digest: bundle_digest.clone(),
            source_digest: Some(bundle.source_digest),
            manifest_json: bundle.manifest_json,
            permissions_json: manifest.permissions,
            limits_json: manifest.limits,
        },
        bundle: WorkflowUploadBundle {
            storage_backend: "inline".to_string(),
            object_key: format!("inline:{}", bundle_digest.replace("sha256:", "")),
            size_bytes: bundle.size_bytes,
            content_base64: Some(BASE64_STANDARD.encode(&bundle_bytes)),
            content_type: Some("text/javascript".to_string()),
            signature: bundle_signature,
        },
        triggers: manifest.triggers,
    };

    let response = client.upload_workflow(&request)?;
    println!("Workflow definition: {}", response.workflow_definition_id);
    println!("Workflow deployment: {}", response.workflow_deployment_id);
    println!("Workflow bundle: {}", response.workflow_bundle_id);
    println!("Activated: {}", response.activated);
    for line in workflow_upload_next_step_lines(&response, &client.context().base_url) {
        println!("{}", line);
    }
    Ok(())
}

fn workflow_upload_next_step_lines(
    response: &WorkflowUploadResponse,
    api_base_url: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(status) = response.workflow_deployment_status.as_deref() {
        lines.push(format!("Deployment status: {}", status));
    }

    if response.review_required {
        if response.review_reasons.is_empty() {
            lines.push("Review required".to_string());
        } else {
            lines.push(format!(
                "Review required: {}",
                response.review_reasons.join(", ")
            ));
        }
        if let Some(organization_id) = response.organization_id.as_deref()
            && let Some(url) = workflow_dashboard_url(api_base_url, organization_id)
        {
            lines.push(format!("Review URL: {}", url));
        }
        lines.push(format!(
            "Approve with: git-ai workflows approve {} {}",
            response.workflow_definition_id, response.workflow_deployment_id
        ));
    } else if response.activated {
        lines.push("Next: deployment is active.".to_string());
    } else if response
        .workflow_deployment_status
        .as_deref()
        .is_some_and(|status| status == "uploaded")
    {
        lines.push(format!(
            "Activate with: git-ai workflows activate {} {}",
            response.workflow_definition_id, response.workflow_deployment_id
        ));
    }

    lines
}

fn workflow_dashboard_url(api_base_url: &str, organization_id: &str) -> Option<String> {
    let mut url = url::Url::parse(api_base_url).ok()?;
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.clear();
        segments.extend(["org", organization_id, "settings", "workflows"]);
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

fn handle_list(args: &[String]) -> Result<(), GitAiError> {
    let options = ListOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    let response = client.list_workflows(options.status.as_deref(), options.limit)?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!(
        "{:<28} {:<24} {:<14} {:<10} {:<12} Updated",
        "ID", "Name", "Status", "Backend", "Version"
    );
    for workflow in response.workflows {
        let backend = workflow
            .current_deployment
            .as_ref()
            .map(|d| d.backend.as_str())
            .unwrap_or("-");
        let version = workflow
            .current_deployment
            .as_ref()
            .map(|d| d.version.as_str())
            .unwrap_or("-");
        println!(
            "{:<28} {:<24} {:<14} {:<10} {:<12} {}",
            workflow.id, workflow.name, workflow.status, backend, version, workflow.updated_at
        );
    }
    Ok(())
}

fn handle_activate(args: &[String]) -> Result<(), GitAiError> {
    let options = DeploymentControlOptions::parse(args, "activate")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.definition.activate")?;
    let response = client.activate_workflow_deployment(
        &options.workflow_definition_id,
        &options.workflow_deployment_id,
    )?;
    println!(
        "Activated workflow deployment {} for definition {} ({})",
        response.workflow_deployment_id, response.workflow_definition_id, response.status
    );
    Ok(())
}

fn handle_approve(args: &[String]) -> Result<(), GitAiError> {
    let options = DeploymentControlOptions::parse(args, "approve")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.definition.review")?;
    let response = client.approve_workflow_deployment_review(
        &options.workflow_definition_id,
        &options.workflow_deployment_id,
    )?;
    println!(
        "Approved workflow deployment {} for definition {} ({})",
        response.workflow_deployment_id, response.workflow_definition_id, response.status
    );
    Ok(())
}

fn handle_reject(args: &[String]) -> Result<(), GitAiError> {
    let options = DeploymentControlOptions::parse(args, "reject")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.definition.review")?;
    let response = client.reject_workflow_deployment_review(
        &options.workflow_definition_id,
        &options.workflow_deployment_id,
    )?;
    println!(
        "Rejected workflow deployment {} for definition {} ({})",
        response.workflow_deployment_id, response.workflow_definition_id, response.status
    );
    Ok(())
}

fn handle_disable(args: &[String]) -> Result<(), GitAiError> {
    let options = DeploymentControlOptions::parse(args, "disable")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.definition.disable")?;
    let response = client.disable_workflow_deployment(
        &options.workflow_definition_id,
        &options.workflow_deployment_id,
    )?;
    println!(
        "Disabled workflow deployment {} for definition {} ({})",
        response.workflow_deployment_id, response.workflow_definition_id, response.status
    );
    Ok(())
}

fn handle_rollback(args: &[String]) -> Result<(), GitAiError> {
    let options = DeploymentControlOptions::parse(args, "rollback")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.definition.rollback")?;
    let response = client.rollback_workflow_deployment(
        &options.workflow_definition_id,
        &options.workflow_deployment_id,
    )?;
    let from = response
        .rolled_back_from_deployment_id
        .as_deref()
        .unwrap_or("unknown");
    println!(
        "Rolled back workflow definition {} from deployment {} to deployment {} ({})",
        response.workflow_definition_id, from, response.workflow_deployment_id, response.status
    );
    Ok(())
}

fn handle_archive(args: &[String]) -> Result<(), GitAiError> {
    let options = DefinitionControlOptions::parse(args, "archive")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.definition.disable")?;
    let response = client.archive_workflow_definition(&options.workflow_definition_id)?;
    println!(
        "Archived workflow definition {} ({}, {} deployment(s))",
        response.workflow_definition_id, response.status, response.archived_deployments
    );
    Ok(())
}

fn handle_restore(args: &[String]) -> Result<(), GitAiError> {
    let options = DefinitionControlOptions::parse(args, "restore")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.definition.write")?;
    let response = client.restore_workflow_definition(&options.workflow_definition_id)?;
    println!(
        "Restored workflow definition {} ({})",
        response.workflow_definition_id, response.status
    );
    Ok(())
}

fn handle_runtime_key(args: &[String]) -> Result<(), GitAiError> {
    match args.first().map(|value| value.as_str()).unwrap_or("help") {
        "rotate" => handle_runtime_key_rotate(&args[1..]),
        "revoke" => handle_runtime_key_revoke(&args[1..]),
        "help" | "--help" | "-h" => {
            print_runtime_key_help();
            Ok(())
        }
        other => Err(GitAiError::Generic(format!(
            "Unknown workflows runtime-key subcommand '{}'",
            other
        ))),
    }
}

fn handle_runtime_key_rotate(args: &[String]) -> Result<(), GitAiError> {
    let options = DeploymentControlOptions::parse(args, "runtime-key rotate")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.runtime_key.rotate")?;
    let response = client.rotate_workflow_deployment_runtime_key(
        &options.workflow_definition_id,
        &options.workflow_deployment_id,
    )?;
    let key = response
        .key
        .as_ref()
        .map(|key| key.id.as_str())
        .unwrap_or("-");
    println!(
        "Rotated runtime key for workflow deployment {} for definition {} (key {}, revoked {})",
        response.workflow_deployment_id, response.workflow_definition_id, key, response.revoked
    );
    Ok(())
}

fn handle_runtime_key_revoke(args: &[String]) -> Result<(), GitAiError> {
    let options = DeploymentControlOptions::parse(args, "runtime-key revoke")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.runtime_key.revoke")?;
    let response = client.revoke_workflow_deployment_runtime_keys(
        &options.workflow_definition_id,
        &options.workflow_deployment_id,
    )?;
    println!(
        "Revoked {} runtime key(s) for workflow deployment {} for definition {}",
        response.revoked, response.workflow_deployment_id, response.workflow_definition_id
    );
    Ok(())
}

fn handle_runs(args: &[String]) -> Result<(), GitAiError> {
    let options = RunsOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    let response = client.list_workflow_runs(
        options.workflow_definition_id.as_deref(),
        options.status.as_deref(),
        options.limit,
    )?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!(
        "{:<28} {:<24} {:<12} {:<10} {:<16} Created",
        "Run", "Workflow", "Status", "Backend", "Trigger"
    );
    for run in response.runs {
        let workflow_name = run
            .definition
            .as_ref()
            .map(|d| d.name.as_str())
            .unwrap_or("-");
        println!(
            "{:<28} {:<24} {:<12} {:<10} {:<16} {}",
            run.id, workflow_name, run.status, run.backend, run.trigger_type, run.created_at
        );
    }
    Ok(())
}

fn handle_inspect(args: &[String]) -> Result<(), GitAiError> {
    let options = InspectOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.run.read")?;
    let run = client.get_workflow_run(&options.run_id)?;

    if options.json {
        println!("{}", serde_json::to_string_pretty(&run)?);
        return Ok(());
    }

    print_workflow_run_detail(&run);
    Ok(())
}

fn handle_logs(args: &[String]) -> Result<(), GitAiError> {
    let options = LogsOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    let mut seen_log_ids = BTreeSet::new();

    loop {
        let mut response =
            client.list_workflow_logs(&options.run_id, options.level.as_deref(), options.limit)?;
        if options.follow {
            response.logs = filter_new_workflow_logs(response.logs, &mut seen_log_ids);
        }
        if options.json {
            if !options.follow || !response.logs.is_empty() {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
        } else {
            for log in workflow_logs_for_terminal_output(response.logs) {
                println!(
                    "{} {:<5} {}{}",
                    log.created_at,
                    log.level,
                    log.message,
                    if log.fields.is_null() {
                        "".to_string()
                    } else {
                        format!(" {}", compact_json(&log.fields))
                    }
                );
            }
        }

        if !options.follow {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    Ok(())
}

fn handle_artifacts(args: &[String]) -> Result<(), GitAiError> {
    let options = ArtifactsOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));

    if let Some(artifact_id) = options.artifact_id {
        ensure_workflow_api_key(&client, "workflow.artifact.read")?;
        let value = client.get_workflow_artifact(&options.run_id, &artifact_id)?;
        let bytes = serde_json::to_vec_pretty(&value)?;
        if let Some(out_path) = options.out_path {
            fs::write(&out_path, with_trailing_newline(bytes))?;
            println!(
                "Wrote workflow artifact {} to {}",
                artifact_id,
                out_path.display()
            );
        } else {
            println!("{}", String::from_utf8_lossy(&bytes));
        }
        return Ok(());
    }

    ensure_workflow_api_key(&client, "workflow.run.read")?;
    let run = client.get_workflow_run(&options.run_id)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": run.summary,
                "artifacts": run.artifacts,
            }))?
        );
        return Ok(());
    }

    if run.artifacts.is_empty() {
        println!("No artifacts for workflow run {}", options.run_id);
        return Ok(());
    }

    println!(
        "{:<28} {:<28} {:<24} {:>10} Created",
        "Artifact", "Step", "Content type", "Size"
    );
    for artifact in run.artifacts {
        print_workflow_artifact_row(&artifact);
    }
    Ok(())
}

fn handle_trigger(args: &[String]) -> Result<(), GitAiError> {
    match args.first().map(|value| value.as_str()).unwrap_or("help") {
        "pr.synchronize" => handle_trigger_pr_synchronize(&args[1..]),
        "help" | "--help" | "-h" => {
            print_trigger_help();
            Ok(())
        }
        other => Err(GitAiError::Generic(format!(
            "Unknown workflows trigger type '{}'",
            other
        ))),
    }
}

fn handle_trigger_pr_synchronize(args: &[String]) -> Result<(), GitAiError> {
    let options = TriggerPrSynchronizeOptions::parse(args)?;
    let raw = fs::read_to_string(&options.fixture_path).map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to read workflow trigger fixture '{}': {}",
            options.fixture_path.display(),
            e
        ))
    })?;
    let event: serde_json::Value = serde_json::from_str(&raw)?;

    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.trigger.write")?;
    let response = client.trigger_pr_synchronize_workflow(
        event,
        options.unique,
        options.idempotency_key_suffix,
    )?;

    if options.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!(
            "Triggered {} event {} ({})",
            response.event_type, response.event_id, response.idempotency_key
        );
    }
    Ok(())
}

fn handle_backfill(args: &[String]) -> Result<(), GitAiError> {
    match args.first().map(|value| value.as_str()).unwrap_or("help") {
        "pr.synchronize" => handle_backfill_pr_synchronize(&args[1..]),
        "help" | "--help" | "-h" => {
            print_backfill_help();
            Ok(())
        }
        other => Err(GitAiError::Generic(format!(
            "Unknown workflows backfill type '{}'",
            other
        ))),
    }
}

fn handle_backfill_pr_synchronize(args: &[String]) -> Result<(), GitAiError> {
    let options = BackfillPrSynchronizeOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.trigger.write")?;

    let request = WorkflowPrSynchronizeBackfillRequest {
        from: options.from,
        to: options.to,
        repositories: none_if_empty(options.repositories),
        providers: none_if_empty(options.providers),
        pr_numbers: none_if_empty(options.pr_numbers),
        limit: options.limit,
        dry_run: Some(options.dry_run),
        idempotency_key_suffix: options.idempotency_key_suffix,
    };
    let response = client.backfill_pr_synchronize_workflow(&request)?;

    if options.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        print_backfill_pr_synchronize_response(&response);
    }
    Ok(())
}

fn handle_cancel(args: &[String]) -> Result<(), GitAiError> {
    let options = ControlRunOptions::parse(args, "cancel")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.run.cancel")?;
    let response = client.cancel_workflow_run(&options.run_id)?;
    println!(
        "Accepted {} for workflow run {} ({})",
        response.action, response.run_id, response.backend
    );
    Ok(())
}

fn handle_refresh(args: &[String]) -> Result<(), GitAiError> {
    let options = ControlRunOptions::parse(args, "refresh")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.run.read")?;
    let response = client.refresh_workflow_run(&options.run_id)?;
    println!(
        "Refreshed workflow run {}: {} ({}, {})",
        response.run_id, response.status, response.backend, response.backend_instance_id
    );
    Ok(())
}

fn handle_restart(args: &[String]) -> Result<(), GitAiError> {
    let options = ControlRunOptions::parse(args, "restart")?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.run.restart")?;
    let response = client.restart_workflow_run(&options.run_id, options.from_step)?;
    println!(
        "Accepted {} for workflow run {} ({})",
        response.action, response.run_id, response.backend
    );
    Ok(())
}

fn handle_secrets(args: &[String]) -> Result<(), GitAiError> {
    match args.first().map(|value| value.as_str()).unwrap_or("help") {
        "list" => handle_secrets_list(&args[1..]),
        "set" => handle_secrets_set(&args[1..]),
        "delete" | "rm" => handle_secrets_delete(&args[1..]),
        "help" | "--help" | "-h" => {
            print_secrets_help();
            Ok(())
        }
        other => Err(GitAiError::Generic(format!(
            "Unknown workflows secrets subcommand '{}'",
            other
        ))),
    }
}

fn handle_secrets_list(args: &[String]) -> Result<(), GitAiError> {
    let options = SecretsListOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.secret.read")?;
    let response = client.list_workflow_secrets()?;

    if options.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("{:<32} {:<28} Updated", "Name", "Created");
    for secret in response.secrets {
        println!(
            "{:<32} {:<28} {}",
            secret.name, secret.created_at, secret.updated_at
        );
    }
    Ok(())
}

fn handle_secrets_set(args: &[String]) -> Result<(), GitAiError> {
    let options = SecretsSetOptions::parse(args)?;
    validate_workflow_secret_name(&options.name)?;
    let value = match options.value {
        SecretValueInput::Literal(value) => value,
        SecretValueInput::Stdin => {
            let mut value = String::new();
            io::stdin().read_to_string(&mut value)?;
            strip_single_trailing_newline(value)
        }
    };
    if value.is_empty() {
        return Err(GitAiError::Generic(
            "workflow secret value must not be empty".to_string(),
        ));
    }

    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.secret.write")?;
    let response = client.set_workflow_secret(&options.name, &value)?;

    println!(
        "{} workflow secret {}",
        if response.created {
            "Created"
        } else {
            "Updated"
        },
        response.secret.name
    );
    Ok(())
}

fn handle_secrets_delete(args: &[String]) -> Result<(), GitAiError> {
    let options = SecretsDeleteOptions::parse(args)?;
    validate_workflow_secret_name(&options.name)?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.secret.delete")?;
    let response = client.delete_workflow_secret(&options.name)?;
    if response.deleted {
        println!("Deleted workflow secret {}", options.name);
    } else {
        println!("Workflow secret {} was not found", options.name);
    }
    Ok(())
}

fn handle_notifications(args: &[String]) -> Result<(), GitAiError> {
    match args.first().map(|value| value.as_str()).unwrap_or("help") {
        "routes" => handle_notification_routes(&args[1..]),
        "help" | "--help" | "-h" => {
            print_notifications_help();
            Ok(())
        }
        other => Err(GitAiError::Generic(format!(
            "Unknown workflows notifications subcommand '{}'",
            other
        ))),
    }
}

fn handle_notification_routes(args: &[String]) -> Result<(), GitAiError> {
    match args.first().map(|value| value.as_str()).unwrap_or("help") {
        "list" => handle_notification_routes_list(&args[1..]),
        "set" => handle_notification_routes_set(&args[1..]),
        "delete" | "rm" => handle_notification_routes_delete(&args[1..]),
        "help" | "--help" | "-h" => {
            print_notification_routes_help();
            Ok(())
        }
        other => Err(GitAiError::Generic(format!(
            "Unknown workflows notifications routes subcommand '{}'",
            other
        ))),
    }
}

fn handle_notification_routes_list(args: &[String]) -> Result<(), GitAiError> {
    let options = NotificationRoutesListOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.notification.read")?;
    let response = client.list_workflow_notification_routes()?;

    if options.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!(
        "{:<28} {:<18} {:<16} {:<28} Updated",
        "Channel", "Transport", "Enabled", "Target"
    );
    for route in response.routes {
        print_workflow_notification_route_row(&route);
    }
    Ok(())
}

fn handle_notification_routes_set(args: &[String]) -> Result<(), GitAiError> {
    let options = NotificationRoutesSetOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.notification.write")?;
    let response = client.set_workflow_notification_route(
        &options.channel,
        Some(&options.transport),
        options.target_url.as_deref(),
        options.enabled,
    )?;

    println!(
        "{} workflow notification route {} ({}, {})",
        if response.created {
            "Created"
        } else {
            "Updated"
        },
        response.route.channel,
        response.route.transport,
        if response.route.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    Ok(())
}

fn handle_notification_routes_delete(args: &[String]) -> Result<(), GitAiError> {
    let options = NotificationRoutesDeleteOptions::parse(args)?;
    let client = ApiClient::new(ApiContext::new(None));
    ensure_workflow_api_key(&client, "workflow.notification.delete")?;
    let response = client.delete_workflow_notification_route(&options.channel)?;
    if response.deleted {
        println!("Deleted workflow notification route {}", options.channel);
    } else {
        println!(
            "Workflow notification route {} was not found",
            options.channel
        );
    }
    Ok(())
}

fn bundle_workflow(
    manifest_path: Option<&Path>,
    out_dir: Option<&Path>,
) -> Result<BundleOutput, GitAiError> {
    bundle_workflow_with_mode(manifest_path, out_dir, BundleMode::NodeEsbuild)
}

fn bundle_workflow_with_mode(
    manifest_path: Option<&Path>,
    out_dir: Option<&Path>,
    mode: BundleMode,
) -> Result<BundleOutput, GitAiError> {
    let (manifest, manifest_json, resolved_manifest_path) = read_manifest(manifest_path)?;
    validate_manifest(&manifest, &manifest_json, &resolved_manifest_path)?;
    let manifest_dir = parent_dir_or_current(&resolved_manifest_path);
    let bundle_manifest_json =
        manifest_json_with_supply_chain_metadata(&manifest, &manifest_json, manifest_dir, mode)?;
    let entrypoint_path = manifest_dir.join(&manifest.entrypoint);
    let source_bytes = fs::read(&entrypoint_path).map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to read workflow entrypoint '{}': {}",
            entrypoint_path.display(),
            e
        ))
    })?;
    let output_dir = out_dir.map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(".gitai/workflows")
            .join(&manifest.slug)
            .join(&manifest.version)
    });
    fs::create_dir_all(&output_dir)?;

    let bundle_path = output_dir.join("bundle.js");
    let output_manifest_path = output_dir.join("manifest.json");
    match mode {
        BundleMode::NodeEsbuild => {
            run_esbuild_bundle(&manifest_dir, &entrypoint_path, &bundle_path)?;
        }
        BundleMode::CopyEntrypoint => {
            fs::write(&bundle_path, &source_bytes)?;
        }
    }
    fs::write(
        &output_manifest_path,
        serde_json::to_vec_pretty(&bundle_manifest_json)?,
    )?;

    let bundle_bytes = fs::read(&bundle_path)?;
    validate_workflow_bundle_size(bundle_bytes.len() as u64, &bundle_path)?;
    let source_digest = sha256_hex(&source_bytes);
    let bundle_digest = bundle_digest(&bundle_bytes, &bundle_manifest_json)?;
    fs::write(output_dir.join("source-digest.txt"), &source_digest)?;
    fs::write(output_dir.join("bundle-digest.txt"), &bundle_digest)?;

    Ok(BundleOutput {
        output_dir,
        bundle_path,
        manifest_path: output_manifest_path,
        manifest_json: bundle_manifest_json,
        source_digest,
        bundle_digest,
        size_bytes: bundle_bytes.len() as u64,
    })
}

fn run_workflow_dev(options: DevOptions) -> Result<(), GitAiError> {
    if options.watch {
        return run_workflow_dev_watch(options);
    }
    run_workflow_dev_once(&options).map(|_| ())
}

#[derive(Debug, Clone)]
struct DevRunContext {
    manifest_dir: PathBuf,
    manifest_path: PathBuf,
    entrypoint_path: PathBuf,
    event_path: PathBuf,
}

fn run_workflow_dev_once(options: &DevOptions) -> Result<DevRunContext, GitAiError> {
    let (manifest, manifest_json, manifest_path) = read_manifest(options.manifest.as_deref())?;
    validate_manifest(&manifest, &manifest_json, &manifest_path)?;
    let manifest_dir = parent_dir_or_current(&manifest_path);
    let event_path = options
        .event_path
        .clone()
        .unwrap_or_else(|| manifest_dir.join("fixtures/pr.synchronize.json"));
    if !event_path.exists() {
        return Err(GitAiError::Generic(format!(
            "workflow dev event fixture '{}' does not exist",
            event_path.display()
        )));
    }
    let entrypoint_path = manifest_dir.join(&manifest.entrypoint);

    run_workflow_dev_entrypoint(manifest_dir, &entrypoint_path, &event_path, options.json)?;

    Ok(DevRunContext {
        manifest_dir: manifest_dir.to_path_buf(),
        manifest_path,
        entrypoint_path,
        event_path,
    })
}

fn run_workflow_dev_watch(options: DevOptions) -> Result<(), GitAiError> {
    println!("Watching workflow files. Press Ctrl+C to stop.");
    let mut context = run_workflow_dev_once(&options)?;
    let mut snapshot = workflow_dev_watch_snapshot(&context);

    loop {
        thread::sleep(Duration::from_millis(WORKFLOW_DEV_WATCH_POLL_MS));
        let next_snapshot = workflow_dev_watch_snapshot(&context);
        if next_snapshot == snapshot {
            continue;
        }

        println!("Detected workflow change; rerunning dev fixture.");
        match run_workflow_dev_once(&options) {
            Ok(next_context) => {
                context = next_context;
                snapshot = workflow_dev_watch_snapshot(&context);
            }
            Err(error) => {
                eprintln!("Workflow dev run failed: {}", error);
                snapshot = next_snapshot;
            }
        }
    }
}

fn run_workflow_dev_entrypoint(
    manifest_dir: &Path,
    entrypoint_path: &Path,
    event_path: &Path,
    json_output: bool,
) -> Result<(), GitAiError> {
    let runner_dir = manifest_dir.join(".gitai/workflows/dev");
    fs::create_dir_all(&runner_dir)?;
    let runner_path = runner_dir.join("runner.mjs");
    fs::write(
        &runner_path,
        dev_runner_source(entrypoint_path, event_path, json_output)?,
    )?;

    run_node_tool(
        manifest_dir,
        "tsx",
        "tsx@4.20.6",
        &[canonical_display_path(&runner_path)],
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DevWatchFileState {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: Option<u64>,
    exists: bool,
}

fn workflow_dev_watch_snapshot(context: &DevRunContext) -> Vec<DevWatchFileState> {
    let mut paths = BTreeSet::new();
    paths.insert(context.manifest_path.clone());
    paths.insert(context.entrypoint_path.clone());
    paths.insert(context.event_path.clone());
    collect_workflow_dev_watch_paths(&context.manifest_dir, &mut paths);

    paths
        .into_iter()
        .map(|path| dev_watch_file_state(&path))
        .collect()
}

fn dev_watch_file_state(path: &Path) -> DevWatchFileState {
    match fs::metadata(path) {
        Ok(metadata) => DevWatchFileState {
            path: path.to_path_buf(),
            modified: metadata.modified().ok(),
            len: Some(metadata.len()),
            exists: true,
        },
        Err(_) => DevWatchFileState {
            path: path.to_path_buf(),
            modified: None,
            len: None,
            exists: false,
        },
    }
}

fn collect_workflow_dev_watch_paths(dir: &Path, paths: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_workflow_dev_watch_dir(&path) {
                continue;
            }
            collect_workflow_dev_watch_paths(&path, paths);
            continue;
        }

        if is_workflow_dev_watch_file(&path) {
            paths.insert(path);
        }
    }
}

fn should_skip_workflow_dev_watch_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git" | ".gitai" | "build" | "dist" | "node_modules" | "target"
            )
        })
}

fn is_workflow_dev_watch_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| WORKFLOW_DEV_WATCH_EXTENSIONS.contains(&extension))
}

fn run_esbuild_bundle(
    working_dir: &Path,
    entrypoint_path: &Path,
    bundle_path: &Path,
) -> Result<(), GitAiError> {
    let args = vec![
        canonical_display_path(entrypoint_path),
        "--bundle".to_string(),
        "--platform=node".to_string(),
        "--format=esm".to_string(),
        "--target=es2022".to_string(),
        "--external:@git-ai-project/workflows".to_string(),
        format!("--outfile={}", canonical_display_path(bundle_path)),
    ];
    run_node_tool(working_dir, "esbuild", "esbuild@0.25.0", &args)
}

fn run_node_tool(
    working_dir: &Path,
    local_tool_name: &str,
    npx_package: &str,
    args: &[String],
) -> Result<(), GitAiError> {
    let local_tool = node_modules_bin(working_dir, local_tool_name);
    let mut command = if local_tool.exists() {
        let mut command = Command::new(local_tool);
        command.args(args);
        command
    } else if let (Some(node_path), Some(npx_path)) = (
        find_executable_on_path("node"),
        find_executable_on_path("npx"),
    ) {
        let mut command = Command::new(node_path);
        command
            .arg(npx_path)
            .arg("--yes")
            .arg(npx_package)
            .args(args);
        command
    } else if let (Some(node_path), Some(npm_path)) = (
        find_executable_on_path("node"),
        find_executable_on_path("npm"),
    ) {
        let mut command = Command::new(node_path);
        command
            .arg(npm_path)
            .arg("exec")
            .arg("--yes")
            .arg("--package")
            .arg(npx_package)
            .arg("--")
            .arg(local_tool_name)
            .args(args);
        command
    } else {
        return Err(GitAiError::Generic(format!(
            "{} requires Node.js/npm. Install Node.js and run npm install in the workflow project.",
            local_tool_name
        )));
    };
    command.current_dir(working_dir);

    let output = command.output().map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to execute {}. Install Node.js/npm or run npm install in the workflow project: {}",
            local_tool_name, e
        ))
    })?;
    if !output.status.success() {
        return Err(GitAiError::Generic(format!(
            "{} failed:\n{}{}",
            local_tool_name,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        print!("{}", stdout);
    }
    Ok(())
}

fn node_modules_bin(working_dir: &Path, name: &str) -> PathBuf {
    let executable = if cfg!(windows) {
        format!("{}.cmd", name)
    } else {
        name.to_string()
    };
    working_dir
        .join("node_modules")
        .join(".bin")
        .join(executable)
}

fn parent_dir_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn find_executable_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            let candidate = entry.join(format!("{}.cmd", name));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn dev_runner_source(
    entrypoint_path: &Path,
    event_path: &Path,
    json_output: bool,
) -> Result<String, GitAiError> {
    let entrypoint = serde_json::to_string(&canonical_display_path(entrypoint_path))?;
    let event = serde_json::to_string(&canonical_display_path(event_path))?;
    let json = if json_output { "true" } else { "false" };
    Ok(format!(
        r#"import {{ readFile }} from "node:fs/promises";
import {{ pathToFileURL }} from "node:url";
import {{ runWorkflowForTest }} from "@git-ai-project/workflows/testing";

const workflowModule = await import(pathToFileURL({entrypoint}).href);
const workflow = workflowModule.default;
if (!workflow || typeof workflow.run !== "function") {{
  throw new Error("Workflow module must export a default workflow definition");
}}

const event = JSON.parse(await readFile({event}, "utf8"));
const logs = [];
const logger = {{
  debug(message, fields) {{ logs.push({{ level: "debug", message, fields }}); }},
  info(message, fields) {{ logs.push({{ level: "info", message, fields }}); }},
  warn(message, fields) {{ logs.push({{ level: "warn", message, fields }}); }},
  error(message, fields) {{ logs.push({{ level: "error", message, fields }}); }},
}};

const result = await runWorkflowForTest(workflow, {{ event, logger }});
const output = redact({{ ...result, logs }});

if ({json}) {{
  console.log(JSON.stringify(output, null, 2));
}} else {{
  console.log(`Workflow completed with ${{output.steps.length}} step(s)`);
  for (const step of output.steps) {{
    console.log(`- ${{step.type}} ${{step.name}}`);
  }}
  if (output.notifications.length > 0) {{
    console.log(`Notifications: ${{output.notifications.length}}`);
  }}
  if (output.logs.length > 0) {{
    console.log("Logs:");
    for (const log of output.logs) {{
      console.log(`- ${{log.level}} ${{log.message}} ${{log.fields ? JSON.stringify(log.fields) : ""}}`);
    }}
  }}
  console.log("Result:");
  console.log(JSON.stringify(output.result, null, 2));
}}

function redact(value, depth = 0) {{
  if (depth > 20) return "[REDACTED_DEPTH]";
  if (typeof value === "string") return redactString(value);
  if (Array.isArray(value)) return value.map((entry) => redact(entry, depth + 1));
  if (value && typeof value === "object") {{
    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [
        key,
        isSensitiveKey(key) ? "[REDACTED]" : redact(entry, depth + 1),
      ]),
    );
  }}
  return value;
}}

function redactString(value) {{
  return value
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer [REDACTED]")
    .replace(/gh[opsru]_[A-Za-z0-9_]{{20,}}/g, "[REDACTED]")
    .replace(/glpat-[A-Za-z0-9_-]{{20,}}/g, "[REDACTED]")
    .replace(/xox[baprs]-[A-Za-z0-9-]{{20,}}/g, "[REDACTED]");
}}

function isSensitiveKey(key) {{
  return /token|secret|password|authorization|api[_-]?key/i.test(key);
}}
"#,
        entrypoint = entrypoint,
        event = event,
        json = json,
    ))
}

fn bundle_from_existing(
    manifest: &LocalWorkflowManifest,
    manifest_json: &serde_json::Value,
    manifest_dir: &Path,
    bundle_path: &Path,
) -> Result<BundleOutput, GitAiError> {
    let bundle_bytes = fs::read(bundle_path).map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to read workflow bundle '{}': {}",
            bundle_path.display(),
            e
        ))
    })?;
    validate_workflow_bundle_size(bundle_bytes.len() as u64, bundle_path)?;
    let bundle_manifest_json = manifest_json_with_supply_chain_metadata(
        manifest,
        manifest_json,
        manifest_dir,
        BundleMode::NodeEsbuild,
    )?;
    let output_dir = bundle_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let manifest_path = output_dir.join("manifest.json");
    let bundle_manifest_json = if manifest_path.exists() {
        let stored_manifest_json: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).map_err(|e| {
                GitAiError::Generic(format!(
                    "Failed to read workflow bundle manifest '{}': {}",
                    manifest_path.display(),
                    e
                ))
            })?)?;
        if stored_manifest_json != bundle_manifest_json {
            return Err(GitAiError::Generic(format!(
                "prebuilt workflow bundle manifest '{}' does not match {}; rebuild the bundle before upload",
                manifest_path.display(),
                DEFAULT_MANIFEST_PATH
            )));
        }
        stored_manifest_json
    } else {
        bundle_manifest_json
    };
    let bundle_digest = bundle_digest(&bundle_bytes, &bundle_manifest_json)?;
    if let Some(declared_digest) =
        read_optional_workflow_digest(&output_dir.join("bundle-digest.txt"))?
        && declared_digest != bundle_digest
    {
        return Err(GitAiError::Generic(format!(
            "workflow bundle digest file '{}' does not match the bundle bytes and manifest; rebuild the bundle before upload",
            output_dir.join("bundle-digest.txt").display()
        )));
    }
    let source_digest = read_optional_workflow_digest(&output_dir.join("source-digest.txt"))?
        .unwrap_or_else(|| sha256_hex(&bundle_bytes));

    Ok(BundleOutput {
        output_dir,
        bundle_path: bundle_path.to_path_buf(),
        manifest_path,
        source_digest,
        bundle_digest,
        manifest_json: bundle_manifest_json,
        size_bytes: bundle_bytes.len() as u64,
    })
}

fn read_optional_workflow_digest(path: &Path) -> Result<Option<String>, GitAiError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GitAiError::Generic(format!(
                "Failed to read workflow digest '{}': {}",
                path.display(),
                error
            )));
        }
    };
    let digest = raw.trim();
    if !digest.starts_with("sha256:") || digest.len() <= "sha256:".len() {
        return Err(GitAiError::Generic(format!(
            "workflow digest '{}' must start with sha256:",
            path.display()
        )));
    }
    Ok(Some(digest.to_string()))
}

fn read_manifest(
    manifest_path: Option<&Path>,
) -> Result<(LocalWorkflowManifest, serde_json::Value, PathBuf), GitAiError> {
    let path = manifest_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MANIFEST_PATH));
    let raw = fs::read_to_string(&path).map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to read workflow manifest '{}': {}",
            path.display(),
            e
        ))
    })?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone())?;
    Ok((manifest, value, path))
}

fn validate_manifest(
    manifest: &LocalWorkflowManifest,
    manifest_json: &serde_json::Value,
    manifest_path: &Path,
) -> Result<(), GitAiError> {
    if manifest.schema_version != "workflow-manifest/1.0" {
        return Err(GitAiError::Generic(
            "workflow manifest schemaVersion must be workflow-manifest/1.0".to_string(),
        ));
    }
    if !is_valid_slug(&manifest.slug) {
        return Err(GitAiError::Generic(
            "workflow manifest slug must be 1-63 chars of lowercase letters, numbers, or hyphens"
                .to_string(),
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err(GitAiError::Generic(
            "workflow manifest name is required".to_string(),
        ));
    }
    if manifest.version.trim().is_empty() {
        return Err(GitAiError::Generic(
            "workflow manifest version is required".to_string(),
        ));
    }
    if manifest.entrypoint.trim().is_empty() {
        return Err(GitAiError::Generic(
            "workflow manifest entrypoint is required".to_string(),
        ));
    }
    if !SUPPORTED_WORKFLOW_RUNTIMES.contains(&manifest.runtime.as_str()) {
        return Err(GitAiError::Generic(
            "workflow manifest runtime must be node18, node20, or node22".to_string(),
        ));
    }
    if manifest.backend != "bullmq" && manifest.backend != "cloudflare" {
        return Err(GitAiError::Generic(
            "workflow manifest backend must be bullmq or cloudflare".to_string(),
        ));
    }
    validate_local_workflow_sdk_manifest(manifest)?;
    if manifest.triggers.is_empty() {
        return Err(GitAiError::Generic(
            "workflow manifest must define at least one trigger".to_string(),
        ));
    }
    for trigger in &manifest.triggers {
        if trigger.trigger_type != "pr.synchronize" {
            return Err(GitAiError::Generic(format!(
                "unsupported workflow trigger '{}'",
                trigger.trigger_type
            )));
        }
        validate_pr_synchronize_filter(&trigger.filter)?;
    }

    let manifest_dir = parent_dir_or_current(manifest_path);
    let entrypoint_path = manifest_dir.join(&manifest.entrypoint);
    if !entrypoint_path.exists() {
        return Err(GitAiError::Generic(format!(
            "workflow entrypoint '{}' does not exist",
            entrypoint_path.display()
        )));
    }
    let entrypoint_source = fs::read_to_string(&entrypoint_path).map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to read workflow entrypoint '{}': {}",
            entrypoint_path.display(),
            e
        ))
    })?;
    validate_entrypoint_default_export(&entrypoint_source, &entrypoint_path)?;
    if manifest_json.get("permissions").is_none() {
        return Err(GitAiError::Generic(
            "workflow manifest permissions are required".to_string(),
        ));
    }
    validate_permissions(&manifest.permissions)?;
    validate_limits(&manifest.limits)?;
    Ok(())
}

fn validate_pr_synchronize_filter(filter: &serde_json::Value) -> Result<(), GitAiError> {
    let object = expect_json_object(filter, "workflow trigger filter")?;
    validate_optional_string_array(object.get("repositories"), "repositories")?;
    validate_optional_string_array(object.get("branches"), "branches")?;
    if let Some(states) = validate_optional_string_array(object.get("states"), "states")? {
        for state in states {
            if state != "open" && state != "closed" && state != "merged" {
                return Err(GitAiError::Generic(format!(
                    "unsupported pr.synchronize state '{}'",
                    state
                )));
            }
        }
    }
    validate_optional_boolean(
        object.get("defaultBranchMergesOnly"),
        "defaultBranchMergesOnly",
    )?;
    validate_optional_boolean(object.get("materialChangesOnly"), "materialChangesOnly")?;

    for key in object.keys() {
        if ![
            "repositories",
            "branches",
            "states",
            "defaultBranchMergesOnly",
            "materialChangesOnly",
        ]
        .contains(&key.as_str())
        {
            return Err(GitAiError::Generic(format!(
                "unsupported pr.synchronize filter field '{}'",
                key
            )));
        }
    }

    Ok(())
}

fn validate_permissions(value: &serde_json::Value) -> Result<(), GitAiError> {
    let object = expect_json_object(value, "workflow manifest permissions")?;
    for key in object.keys() {
        if !["scm", "gitAi", "network", "secrets"].contains(&key.as_str()) {
            return Err(GitAiError::Generic(format!(
                "unsupported workflow permission group '{}'",
                key
            )));
        }
    }

    validate_permission_values(object.get("scm"), "scm", SUPPORTED_SCM_PERMISSIONS)?;
    validate_permission_values(object.get("gitAi"), "gitAi", SUPPORTED_GIT_AI_PERMISSIONS)?;
    for secret in
        validate_optional_string_array(object.get("secrets"), "secrets")?.unwrap_or_default()
    {
        validate_workflow_secret_name(secret)?;
    }
    for pattern in
        validate_optional_string_array(object.get("network"), "network")?.unwrap_or_default()
    {
        validate_network_permission(pattern)?;
    }

    Ok(())
}

fn validate_local_workflow_sdk_manifest(
    manifest: &LocalWorkflowManifest,
) -> Result<(), GitAiError> {
    if manifest.sdk_package != WORKFLOW_SDK_PACKAGE_NAME {
        return Err(GitAiError::Generic(format!(
            "workflow manifest sdkPackage must be {}",
            WORKFLOW_SDK_PACKAGE_NAME
        )));
    }
    if manifest
        .sdk_version
        .as_ref()
        .map_or(true, |version| version.trim().is_empty())
    {
        return Err(GitAiError::Generic(
            "workflow manifest sdkVersion is required".to_string(),
        ));
    }
    Ok(())
}

fn validate_manifest_sdk_with_server(
    client: &ApiClient,
    manifest: &LocalWorkflowManifest,
) -> Result<(), GitAiError> {
    let capabilities = client.get_workflow_capabilities()?;
    validate_manifest_sdk_policy(manifest, &capabilities.sdk)
}

fn validate_manifest_sdk_policy(
    manifest: &LocalWorkflowManifest,
    sdk: &crate::api::workflows::WorkflowSdkCompatibilityPolicy,
) -> Result<(), GitAiError> {
    if manifest.sdk_package != sdk.sdk_package {
        return Err(GitAiError::Generic(format!(
            "workflow manifest sdkPackage '{}' is not supported by this Git AI server; expected '{}'",
            manifest.sdk_package, sdk.sdk_package
        )));
    }

    let Some(sdk_version) = manifest
        .sdk_version
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Err(GitAiError::Generic(
            "workflow manifest sdkVersion is required for server compatibility checks".to_string(),
        ));
    };

    if !sdk
        .supported_versions
        .iter()
        .any(|version| version == sdk_version)
    {
        return Err(GitAiError::Generic(format!(
            "workflow manifest sdkVersion '{}' is not supported by this Git AI server; supported versions: {}",
            sdk_version,
            sdk.supported_versions.join(", ")
        )));
    }

    Ok(())
}

fn validate_limits(value: &serde_json::Value) -> Result<(), GitAiError> {
    let object = expect_json_object(value, "workflow manifest limits")?;
    let mut redaction_entries = 0usize;
    for (key, value) in object {
        if !SUPPORTED_LIMIT_KEYS.contains(&key.as_str()) {
            return Err(GitAiError::Generic(format!(
                "unsupported workflow limit '{}'",
                key
            )));
        }
        if key == "redactionLiterals" {
            redaction_entries += validate_redaction_limit_entries(
                value,
                "redactionLiterals",
                MAX_WORKFLOW_REDACTION_LITERAL_LENGTH,
            )?;
            continue;
        }
        if key == "redactionPatterns" {
            redaction_entries += validate_redaction_limit_entries(
                value,
                "redactionPatterns",
                MAX_WORKFLOW_REDACTION_PATTERN_LENGTH,
            )?;
            continue;
        }
        if key == "redactionDetectorPacks" {
            for detector_pack in
                validate_optional_string_array(Some(value), "redactionDetectorPacks")?
                    .unwrap_or_default()
            {
                if !SUPPORTED_REDACTION_DETECTOR_PACKS.contains(&detector_pack) {
                    return Err(GitAiError::Generic(format!(
                        "unsupported workflow redaction detector pack '{}'",
                        detector_pack
                    )));
                }
            }
            continue;
        }
        let number = value.as_f64().ok_or_else(|| {
            GitAiError::Generic(format!("workflow limit '{}' must be a number", key))
        })?;
        if !number.is_finite() || number <= 0.0 {
            return Err(GitAiError::Generic(format!(
                "workflow limit '{}' must be greater than zero",
                key
            )));
        }
    }
    if redaction_entries > MAX_WORKFLOW_REDACTION_ENTRIES {
        return Err(GitAiError::Generic(format!(
            "workflow redaction policy supports at most {} entries",
            MAX_WORKFLOW_REDACTION_ENTRIES
        )));
    }
    Ok(())
}

fn validate_redaction_limit_entries(
    value: &serde_json::Value,
    key: &str,
    max_len: usize,
) -> Result<usize, GitAiError> {
    let entries = value.as_array().ok_or_else(|| {
        GitAiError::Generic(format!(
            "workflow limit '{}' must be an array of strings",
            key
        ))
    })?;
    for entry in entries {
        let Some(pattern) = entry.as_str() else {
            return Err(GitAiError::Generic(format!(
                "workflow limit '{}' must be an array of strings",
                key
            )));
        };
        let normalized = pattern.trim();
        if normalized.is_empty() {
            return Err(GitAiError::Generic(format!(
                "workflow limit '{}' entries must not be empty",
                key
            )));
        }
        if normalized.len() > max_len {
            return Err(GitAiError::Generic(format!(
                "workflow limit '{}' entries must be {} characters or fewer",
                key, max_len
            )));
        }
        if normalized.contains('\0') {
            return Err(GitAiError::Generic(format!(
                "workflow limit '{}' entries must not contain null bytes",
                key
            )));
        }
    }
    Ok(entries.len())
}

fn validate_permission_values(
    value: Option<&serde_json::Value>,
    group: &str,
    allowed: &[&str],
) -> Result<(), GitAiError> {
    for permission in validate_optional_string_array(value, group)?.unwrap_or_default() {
        if !allowed.contains(&permission) {
            return Err(GitAiError::Generic(format!(
                "unsupported workflow {} permission '{}'",
                group, permission
            )));
        }
    }
    Ok(())
}

fn validate_network_permission(pattern: &str) -> Result<(), GitAiError> {
    if pattern != pattern.trim() || pattern.contains(char::is_whitespace) {
        return Err(GitAiError::Generic(format!(
            "workflow network permission '{}' must not contain whitespace",
            pattern
        )));
    }
    if !(pattern.starts_with("https://") || pattern.starts_with("http://")) {
        return Err(GitAiError::Generic(format!(
            "workflow network permission '{}' must start with http:// or https://",
            pattern
        )));
    }
    if pattern.len() > 512 {
        return Err(GitAiError::Generic(
            "workflow network permission must be 512 characters or fewer".to_string(),
        ));
    }
    Ok(())
}

fn validate_entrypoint_default_export(source: &str, path: &Path) -> Result<(), GitAiError> {
    let count = count_workflow_default_exports(source);
    if count != 1 {
        return Err(GitAiError::Generic(format!(
            "workflow entrypoint '{}' must export exactly one default workflow definition; found {}",
            path.display(),
            count
        )));
    }
    Ok(())
}

fn count_workflow_default_exports(source: &str) -> usize {
    let scrubbed = scrub_js_source_for_export_scan(source);
    let direct_exports = Regex::new(r"\bexport\s+default\b")
        .expect("valid workflow default export regex")
        .find_iter(&scrubbed)
        .count();
    let named_exports = Regex::new(r"\bexport\s*\{([^}]*)\}")
        .expect("valid workflow named export regex")
        .captures_iter(&scrubbed)
        .filter(|captures| {
            captures
                .get(1)
                .is_some_and(|specifiers| contains_default_export_specifier(specifiers.as_str()))
        })
        .count();
    direct_exports + named_exports
}

fn contains_default_export_specifier(specifiers: &str) -> bool {
    specifiers.split(',').any(|specifier| {
        let tokens: Vec<&str> = specifier.split_whitespace().collect();
        matches!(tokens.as_slice(), ["default"])
            || matches!(tokens.as_slice(), [.., "as", "default"])
    })
}

fn scrub_js_source_for_export_scan(source: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Normal,
        LineComment,
        BlockComment,
        SingleQuote,
        DoubleQuote,
        Template,
    }

    let mut output = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut state = State::Normal;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        match state {
            State::Normal => match ch {
                '/' if chars.peek() == Some(&'/') => {
                    output.push(' ');
                    output.push(' ');
                    chars.next();
                    state = State::LineComment;
                }
                '/' if chars.peek() == Some(&'*') => {
                    output.push(' ');
                    output.push(' ');
                    chars.next();
                    state = State::BlockComment;
                }
                '\'' => {
                    output.push(' ');
                    escaped = false;
                    state = State::SingleQuote;
                }
                '"' => {
                    output.push(' ');
                    escaped = false;
                    state = State::DoubleQuote;
                }
                '`' => {
                    output.push(' ');
                    escaped = false;
                    state = State::Template;
                }
                _ => output.push(ch),
            },
            State::LineComment => {
                if ch == '\n' {
                    output.push('\n');
                    state = State::Normal;
                } else {
                    output.push(' ');
                }
            }
            State::BlockComment => {
                if ch == '*' && chars.peek() == Some(&'/') {
                    output.push(' ');
                    output.push(' ');
                    chars.next();
                    state = State::Normal;
                } else if ch == '\n' {
                    output.push('\n');
                } else {
                    output.push(' ');
                }
            }
            State::SingleQuote => {
                output.push(if ch == '\n' { '\n' } else { ' ' });
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '\'' {
                    state = State::Normal;
                }
            }
            State::DoubleQuote => {
                output.push(if ch == '\n' { '\n' } else { ' ' });
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    state = State::Normal;
                }
            }
            State::Template => {
                output.push(if ch == '\n' { '\n' } else { ' ' });
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '`' {
                    state = State::Normal;
                }
            }
        }
    }

    output
}

fn validate_workflow_bundle_size(size_bytes: u64, path: &Path) -> Result<(), GitAiError> {
    validate_workflow_bundle_size_with_limit(size_bytes, path, workflow_bundle_max_bytes())
}

fn validate_workflow_bundle_size_with_limit(
    size_bytes: u64,
    path: &Path,
    max_bytes: u64,
) -> Result<(), GitAiError> {
    if size_bytes > max_bytes {
        return Err(GitAiError::Generic(format!(
            "workflow bundle '{}' is {} bytes, exceeding max size of {} bytes",
            path.display(),
            size_bytes,
            max_bytes
        )));
    }
    Ok(())
}

fn workflow_bundle_max_bytes() -> u64 {
    std::env::var("WORKFLOW_BUNDLE_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_WORKFLOW_BUNDLE_MAX_BYTES)
}

fn expect_json_object<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, GitAiError> {
    value
        .as_object()
        .ok_or_else(|| GitAiError::Generic(format!("{} must be a JSON object", field)))
}

fn validate_optional_string_array<'a>(
    value: Option<&'a serde_json::Value>,
    field: &str,
) -> Result<Option<Vec<&'a str>>, GitAiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let array = value.as_array().ok_or_else(|| {
        GitAiError::Generic(format!(
            "workflow field '{}' must be an array of strings",
            field
        ))
    })?;
    let mut strings = Vec::with_capacity(array.len());
    for entry in array {
        let Some(text) = entry.as_str() else {
            return Err(GitAiError::Generic(format!(
                "workflow field '{}' must be an array of strings",
                field
            )));
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(GitAiError::Generic(format!(
                "workflow field '{}' must not contain empty strings",
                field
            )));
        }
        strings.push(trimmed);
    }
    Ok(Some(strings))
}

fn validate_optional_boolean(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<(), GitAiError> {
    if let Some(value) = value
        && !value.is_boolean()
    {
        return Err(GitAiError::Generic(format!(
            "workflow field '{}' must be a boolean",
            field
        )));
    }
    Ok(())
}

#[derive(Default)]
struct CommonOptions {
    manifest: Option<PathBuf>,
}

impl CommonOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--manifest" if i + 1 < args.len() => {
                    options.manifest = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--help" | "-h" => {
                    print_workflows_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

fn filter_new_workflow_logs(
    logs: Vec<WorkflowLogEntry>,
    seen_log_ids: &mut BTreeSet<String>,
) -> Vec<WorkflowLogEntry> {
    logs.into_iter()
        .filter(|log| seen_log_ids.insert(log.id.clone()))
        .collect()
}

fn workflow_logs_for_terminal_output(mut logs: Vec<WorkflowLogEntry>) -> Vec<WorkflowLogEntry> {
    logs.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    logs
}

#[derive(Default)]
struct DevOptions {
    manifest: Option<PathBuf>,
    event_path: Option<PathBuf>,
    json: bool,
    watch: bool,
}

impl DevOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--manifest" if i + 1 < args.len() => {
                    options.manifest = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--event" if i + 1 < args.len() => {
                    options.event_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--watch" => {
                    options.watch = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_dev_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows dev argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

#[derive(Default)]
struct BundleOptions {
    manifest: Option<PathBuf>,
    out_dir: Option<PathBuf>,
}

impl BundleOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--manifest" if i + 1 < args.len() => {
                    options.manifest = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--out" if i + 1 < args.len() => {
                    options.out_dir = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--help" | "-h" => {
                    print_bundle_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows bundle argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

#[derive(Default)]
struct UploadOptions {
    manifest: Option<PathBuf>,
    bundle_path: Option<PathBuf>,
    backend: Option<String>,
    activate: bool,
    signature_file: Option<PathBuf>,
    signature_key_id: Option<String>,
    signature_algorithm: Option<String>,
}

impl UploadOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--manifest" if i + 1 < args.len() => {
                    options.manifest = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--bundle" if i + 1 < args.len() => {
                    options.bundle_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--backend" if i + 1 < args.len() => {
                    let backend = args[i + 1].clone();
                    if backend != "bullmq" && backend != "cloudflare" {
                        return Err(GitAiError::Generic(
                            "--backend must be bullmq or cloudflare".to_string(),
                        ));
                    }
                    options.backend = Some(backend);
                    i += 2;
                }
                "--signature-file" if i + 1 < args.len() => {
                    options.signature_file = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--signature-key-id" if i + 1 < args.len() => {
                    options.signature_key_id = Some(args[i + 1].clone());
                    i += 2;
                }
                "--signature-algorithm" if i + 1 < args.len() => {
                    options.signature_algorithm = Some(args[i + 1].clone());
                    i += 2;
                }
                "--activate" => {
                    options.activate = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_upload_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows upload argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

fn read_bundle_signature(
    options: &UploadOptions,
) -> Result<Option<WorkflowUploadBundleSignature>, GitAiError> {
    if options.signature_algorithm.is_some()
        && (options.signature_file.is_none() || options.signature_key_id.is_none())
    {
        return Err(GitAiError::Generic(
            "--signature-algorithm requires --signature-file and --signature-key-id".to_string(),
        ));
    }
    match (&options.signature_file, &options.signature_key_id) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(GitAiError::Generic(
            "--signature-key-id is required when --signature-file is set".to_string(),
        )),
        (None, Some(_)) => Err(GitAiError::Generic(
            "--signature-file is required when --signature-key-id is set".to_string(),
        )),
        (Some(path), Some(key_id)) => {
            let algorithm = options
                .signature_algorithm
                .as_deref()
                .unwrap_or("ed25519")
                .trim()
                .to_string();
            if algorithm != "ed25519" {
                return Err(GitAiError::Generic(
                    "--signature-algorithm currently supports only ed25519".to_string(),
                ));
            }
            if key_id.trim().is_empty() {
                return Err(GitAiError::Generic(
                    "--signature-key-id must not be empty".to_string(),
                ));
            }
            let signature = fs::read_to_string(path).map_err(|e| {
                GitAiError::Generic(format!(
                    "Failed to read workflow bundle signature '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            let signature = signature.trim().to_string();
            if signature.is_empty() {
                return Err(GitAiError::Generic(
                    "workflow bundle signature file must not be empty".to_string(),
                ));
            }
            Ok(Some(WorkflowUploadBundleSignature {
                key_id: key_id.trim().to_string(),
                algorithm,
                signature,
            }))
        }
    }
}

#[derive(Default)]
struct ListOptions {
    status: Option<String>,
    limit: Option<u32>,
    json: bool,
}

impl ListOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--status" if i + 1 < args.len() => {
                    options.status = Some(args[i + 1].clone());
                    i += 2;
                }
                "--limit" if i + 1 < args.len() => {
                    options.limit = Some(parse_limit(&args[i + 1])?);
                    i += 2;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_list_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows list argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

struct DefinitionControlOptions {
    workflow_definition_id: String,
}

impl DefinitionControlOptions {
    fn parse(args: &[String], command: &str) -> Result<Self, GitAiError> {
        let Some(workflow_definition_id) = args.first() else {
            return Err(GitAiError::Generic(format!(
                "Usage: git-ai workflows {} <workflow-definition-id>",
                command
            )));
        };
        if workflow_definition_id == "--help" || workflow_definition_id == "-h" {
            print_definition_control_help(command);
            std::process::exit(0);
        }
        if args.len() > 1 {
            return Err(GitAiError::Generic(format!(
                "Unknown workflows {} argument '{}'",
                command, args[1]
            )));
        }

        Ok(Self {
            workflow_definition_id: workflow_definition_id.clone(),
        })
    }
}

struct DeploymentControlOptions {
    workflow_definition_id: String,
    workflow_deployment_id: String,
}

impl DeploymentControlOptions {
    fn parse(args: &[String], command: &str) -> Result<Self, GitAiError> {
        let Some(workflow_definition_id) = args.first() else {
            return Err(GitAiError::Generic(format!(
                "Usage: git-ai workflows {} <workflow-definition-id> <workflow-deployment-id>",
                command
            )));
        };
        if workflow_definition_id == "--help" || workflow_definition_id == "-h" {
            print_deployment_control_help(command);
            std::process::exit(0);
        }
        let Some(workflow_deployment_id) = args.get(1) else {
            return Err(GitAiError::Generic(format!(
                "Usage: git-ai workflows {} <workflow-definition-id> <workflow-deployment-id>",
                command
            )));
        };
        if args.len() > 2 {
            return Err(GitAiError::Generic(format!(
                "Unknown workflows {} argument '{}'",
                command, args[2]
            )));
        }

        Ok(Self {
            workflow_definition_id: workflow_definition_id.clone(),
            workflow_deployment_id: workflow_deployment_id.clone(),
        })
    }
}

#[derive(Default)]
struct RunsOptions {
    workflow_definition_id: Option<String>,
    status: Option<String>,
    limit: Option<u32>,
    json: bool,
}

impl RunsOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        if let Some(first) = args.first()
            && !first.starts_with('-')
        {
            options.workflow_definition_id = Some(first.clone());
            i = 1;
        }
        while i < args.len() {
            match args[i].as_str() {
                "--workflow-definition-id" if i + 1 < args.len() => {
                    options.workflow_definition_id = Some(args[i + 1].clone());
                    i += 2;
                }
                "--status" if i + 1 < args.len() => {
                    options.status = Some(args[i + 1].clone());
                    i += 2;
                }
                "--limit" if i + 1 < args.len() => {
                    options.limit = Some(parse_limit(&args[i + 1])?);
                    i += 2;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_runs_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows runs argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

#[derive(Default)]
struct InspectOptions {
    run_id: String,
    json: bool,
}

impl InspectOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let Some(run_id) = args.first() else {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows inspect <run-id> [--json]".to_string(),
            ));
        };
        if run_id == "--help" || run_id == "-h" {
            print_inspect_help();
            std::process::exit(0);
        }

        let mut options = Self {
            run_id: run_id.clone(),
            ..Self::default()
        };
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_inspect_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows inspect argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

#[derive(Default)]
struct LogsOptions {
    run_id: String,
    level: Option<String>,
    limit: Option<u32>,
    json: bool,
    follow: bool,
}

impl LogsOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let Some(run_id) = args.first() else {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows logs <run-id> [--follow]".to_string(),
            ));
        };
        if run_id == "--help" || run_id == "-h" {
            print_logs_help();
            std::process::exit(0);
        }

        let mut options = Self {
            run_id: run_id.clone(),
            ..Self::default()
        };
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--level" if i + 1 < args.len() => {
                    options.level = Some(args[i + 1].clone());
                    i += 2;
                }
                "--limit" if i + 1 < args.len() => {
                    options.limit = Some(parse_limit(&args[i + 1])?);
                    i += 2;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--follow" | "-f" => {
                    options.follow = true;
                    i += 1;
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows logs argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

#[derive(Default)]
struct ArtifactsOptions {
    run_id: String,
    artifact_id: Option<String>,
    out_path: Option<PathBuf>,
    json: bool,
}

impl ArtifactsOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let Some(run_id) = args.first() else {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows artifacts <run-id> [artifact-id] [--out <path>] [--json]"
                    .to_string(),
            ));
        };
        if run_id == "--help" || run_id == "-h" {
            print_artifacts_help();
            std::process::exit(0);
        }

        let mut options = Self {
            run_id: run_id.clone(),
            ..Self::default()
        };
        let mut i = 1;
        if let Some(next) = args.get(i)
            && !next.starts_with('-')
        {
            options.artifact_id = Some(next.clone());
            i += 1;
        }
        while i < args.len() {
            match args[i].as_str() {
                "--artifact-id" if i + 1 < args.len() => {
                    options.artifact_id = Some(args[i + 1].clone());
                    i += 2;
                }
                "--out" | "-o" if i + 1 < args.len() => {
                    options.out_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_artifacts_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows artifacts argument '{}'",
                        other
                    )));
                }
            }
        }
        if options.out_path.is_some() && options.artifact_id.is_none() {
            return Err(GitAiError::Generic(
                "workflows artifacts --out requires an artifact id".to_string(),
            ));
        }
        Ok(options)
    }
}

#[derive(Default)]
struct TriggerPrSynchronizeOptions {
    fixture_path: PathBuf,
    unique: bool,
    reuse_idempotency_key: bool,
    idempotency_key_suffix: Option<String>,
    json: bool,
}

impl TriggerPrSynchronizeOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self {
            unique: true,
            ..Self::default()
        };
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--fixture" if i + 1 < args.len() => {
                    options.fixture_path = PathBuf::from(&args[i + 1]);
                    i += 2;
                }
                "--idempotency-key-suffix" if i + 1 < args.len() => {
                    options.idempotency_key_suffix = Some(args[i + 1].clone());
                    options.unique = true;
                    i += 2;
                }
                "--reuse-idempotency-key" => {
                    options.reuse_idempotency_key = true;
                    options.unique = false;
                    i += 1;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_trigger_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows trigger pr.synchronize argument '{}'",
                        other
                    )));
                }
            }
        }

        if options.fixture_path.as_os_str().is_empty() {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows trigger pr.synchronize --fixture <file>".to_string(),
            ));
        }
        if options.idempotency_key_suffix.is_some() && options.reuse_idempotency_key {
            return Err(GitAiError::Generic(
                "pass only one of --idempotency-key-suffix or --reuse-idempotency-key".to_string(),
            ));
        }

        Ok(options)
    }
}

#[derive(Default)]
struct BackfillPrSynchronizeOptions {
    from: Option<String>,
    to: Option<String>,
    repositories: Vec<String>,
    providers: Vec<String>,
    pr_numbers: Vec<u32>,
    limit: Option<u32>,
    dry_run: bool,
    idempotency_key_suffix: Option<String>,
    json: bool,
}

impl BackfillPrSynchronizeOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--from" if i + 1 < args.len() => {
                    options.from = Some(args[i + 1].clone());
                    i += 2;
                }
                "--to" if i + 1 < args.len() => {
                    options.to = Some(args[i + 1].clone());
                    i += 2;
                }
                "--repo" | "--repository" if i + 1 < args.len() => {
                    options.repositories.push(args[i + 1].clone());
                    i += 2;
                }
                "--provider" if i + 1 < args.len() => {
                    options
                        .providers
                        .push(normalize_workflow_scm_provider(&args[i + 1])?);
                    i += 2;
                }
                "--pr" | "--pull-request" if i + 1 < args.len() => {
                    options
                        .pr_numbers
                        .push(parse_positive_u32(&args[i + 1], "pull request number")?);
                    i += 2;
                }
                "--limit" if i + 1 < args.len() => {
                    options.limit = Some(parse_limit(&args[i + 1])?);
                    i += 2;
                }
                "--dry-run" => {
                    options.dry_run = true;
                    i += 1;
                }
                "--idempotency-key-suffix" if i + 1 < args.len() => {
                    options.idempotency_key_suffix = Some(args[i + 1].clone());
                    i += 2;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_backfill_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows backfill pr.synchronize argument '{}'",
                        other
                    )));
                }
            }
        }

        if let Some(suffix) = options.idempotency_key_suffix.as_deref() {
            if suffix.chars().any(char::is_whitespace) {
                return Err(GitAiError::Generic(
                    "workflows backfill idempotency key suffix must not contain whitespace"
                        .to_string(),
                ));
            }
        }

        Ok(options)
    }
}

#[derive(Default)]
struct ControlRunOptions {
    run_id: String,
    from_step: Option<String>,
}

impl ControlRunOptions {
    fn parse(args: &[String], command: &str) -> Result<Self, GitAiError> {
        let Some(run_id) = args.first() else {
            return Err(GitAiError::Generic(format!(
                "Usage: git-ai workflows {} <run-id>",
                command
            )));
        };
        if run_id == "--help" || run_id == "-h" {
            print_control_help(command);
            std::process::exit(0);
        }

        let mut options = Self {
            run_id: run_id.clone(),
            ..Self::default()
        };
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--from-step" if command == "restart" && i + 1 < args.len() => {
                    options.from_step = Some(args[i + 1].clone());
                    i += 2;
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows {} argument '{}'",
                        command, other
                    )));
                }
            }
        }
        Ok(options)
    }
}

fn parse_limit(value: &str) -> Result<u32, GitAiError> {
    let parsed = value.parse::<u32>().map_err(|_| {
        GitAiError::Generic(format!("limit must be a positive integer, got '{}'", value))
    })?;
    if parsed == 0 {
        return Err(GitAiError::Generic(
            "limit must be a positive integer".to_string(),
        ));
    }
    Ok(parsed)
}

fn parse_positive_u32(value: &str, label: &str) -> Result<u32, GitAiError> {
    let parsed = value.parse::<u32>().map_err(|_| {
        GitAiError::Generic(format!(
            "{} must be a positive integer, got '{}'",
            label, value
        ))
    })?;
    if parsed == 0 {
        return Err(GitAiError::Generic(format!(
            "{} must be a positive integer",
            label
        )));
    }
    Ok(parsed)
}

fn none_if_empty<T>(values: Vec<T>) -> Option<Vec<T>> {
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn ensure_workflow_api_key(client: &ApiClient, permission: &str) -> Result<(), GitAiError> {
    if client.has_api_key() {
        Ok(())
    } else {
        Err(GitAiError::Generic(format!(
            "workflow command requires config.api_key with {}",
            permission
        )))
    }
}

fn with_trailing_newline(mut bytes: Vec<u8>) -> Vec<u8> {
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes
}

fn validate_workflow_secret_name(name: &str) -> Result<(), GitAiError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(workflow_secret_name_error());
    };
    if name.len() > 128 || !(first.is_ascii_alphabetic() || first == '_') {
        return Err(workflow_secret_name_error());
    }
    if chars.any(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-')) {
        return Err(workflow_secret_name_error());
    }
    Ok(())
}

fn normalize_workflow_scm_provider(provider: &str) -> Result<String, GitAiError> {
    match provider {
        "github" | "gitlab" | "bitbucket" | "azure-devops" => Ok(provider.to_string()),
        "ado" => Ok("azure-devops".to_string()),
        _ => Err(GitAiError::Generic(format!(
            "unsupported workflow SCM provider '{}'",
            provider
        ))),
    }
}

fn normalize_workflow_notification_transport(transport: &str) -> Result<String, GitAiError> {
    match transport {
        "webhook" | "email" | "scm_pr_comment" => Ok(transport.to_string()),
        "scm-pr-comment" => Ok("scm_pr_comment".to_string()),
        _ => Err(GitAiError::Generic(format!(
            "unsupported workflow notification transport '{}'",
            transport
        ))),
    }
}

fn workflow_secret_name_error() -> GitAiError {
    GitAiError::Generic(
        "workflow secret name must be 1-128 chars, start with a letter or underscore, and contain only letters, numbers, underscores, dots, or hyphens".to_string(),
    )
}

fn strip_single_trailing_newline(mut value: String) -> String {
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    value
}

fn default_backend() -> String {
    "bullmq".to_string()
}

fn default_sdk_package() -> String {
    "@git-ai-project/workflows".to_string()
}

fn write_new_file(path: PathBuf, contents: String) -> Result<(), GitAiError> {
    if path.exists() {
        return Err(GitAiError::Generic(format!(
            "Refusing to overwrite existing file '{}'",
            path.display()
        )));
    }
    let mut file = fs::File::create(&path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn normalize_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.chars().flat_map(|ch| ch.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 63 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "workflow".to_string()
    } else {
        slug
    }
}

fn is_valid_slug(value: &str) -> bool {
    let len = value.len();
    len > 0
        && len <= 63
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && value
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn bundle_digest(
    bundle_bytes: &[u8],
    manifest_json: &serde_json::Value,
) -> Result<String, GitAiError> {
    let mut hasher = Sha256::new();
    hasher.update(bundle_bytes);
    hasher.update(b"\n");
    hasher.update(serde_json::to_vec(manifest_json)?);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn manifest_json_with_supply_chain_metadata(
    manifest: &LocalWorkflowManifest,
    manifest_json: &serde_json::Value,
    manifest_dir: &Path,
    mode: BundleMode,
) -> Result<serde_json::Value, GitAiError> {
    let mut value = manifest_json.clone();
    let Some(object) = value.as_object_mut() else {
        return Ok(value);
    };

    let mut supply_chain = object
        .get("supplyChain")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    supply_chain.insert(
        "build".to_string(),
        serde_json::json!({
            "cli": {
                "name": "git-ai",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "sdk": {
                "package": manifest.sdk_package.as_str(),
                "version": manifest.sdk_version.as_deref().unwrap_or(""),
            },
            "bundle": bundle_tool_metadata(mode),
            "lockfiles": workflow_lockfile_metadata(manifest_dir)?,
        }),
    );
    object.insert(
        "supplyChain".to_string(),
        serde_json::Value::Object(supply_chain),
    );
    Ok(value)
}

fn bundle_tool_metadata(mode: BundleMode) -> serde_json::Value {
    match mode {
        BundleMode::NodeEsbuild => serde_json::json!({
            "mode": "node-esbuild",
            "tool": "esbuild",
            "toolVersion": "0.25.0",
            "format": "esm",
            "platform": "node",
            "target": "es2022",
        }),
        BundleMode::CopyEntrypoint => serde_json::json!({
            "mode": "copy-entrypoint",
            "tool": "copy",
        }),
    }
}

fn workflow_lockfile_metadata(manifest_dir: &Path) -> Result<Vec<serde_json::Value>, GitAiError> {
    const LOCKFILES: &[&str] = &[
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
    ];

    let mut lockfiles = Vec::new();
    for relative_path in LOCKFILES {
        let path = manifest_dir.join(relative_path);
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to read workflow dependency lockfile '{}': {}",
                path.display(),
                e
            ))
        })?;
        lockfiles.push(serde_json::json!({
            "path": relative_path,
            "digest": sha256_hex(&bytes),
            "sizeBytes": bytes.len(),
        }));
    }
    Ok(lockfiles)
}

fn canonical_display_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn print_workflow_artifact_row(artifact: &WorkflowArtifactSummary) {
    println!(
        "{:<28} {:<28} {:<24} {:>10} {}",
        artifact.id,
        artifact.step_id.as_deref().unwrap_or("-"),
        artifact.content_type,
        artifact.size_bytes,
        artifact.created_at
    );
}

fn print_workflow_token_lease_row(lease: &WorkflowRunTokenLeaseSummary) {
    let permissions = if lease.requested_permissions.is_empty() {
        "-".to_string()
    } else {
        lease.requested_permissions.join(",")
    };
    println!(
        "{:<28} {:<12} {:<28} {:<28} {:<24} {}",
        lease.id,
        lease.provider,
        lease.step_id.as_deref().unwrap_or("run"),
        lease.repo_id.as_deref().unwrap_or("-"),
        permissions,
        lease.expires_at
    );
}

fn print_workflow_run_detail(run: &WorkflowRunDetail) {
    let workflow_name = run
        .summary
        .definition
        .as_ref()
        .map(|definition| definition.name.as_str())
        .unwrap_or("-");
    println!("Run: {}", run.summary.id);
    println!("Workflow: {}", workflow_name);
    println!("Status: {}", run.summary.status);
    println!("Backend: {}", run.summary.backend);
    println!("Trigger: {}", run.summary.trigger_type);
    println!("Attempt: {}", run.summary.attempt);
    println!("Created: {}", run.summary.created_at);
    if let Some(started_at) = &run.summary.started_at {
        println!("Started: {}", started_at);
    }
    if let Some(completed_at) = &run.summary.completed_at {
        println!("Completed: {}", completed_at);
    }

    println!();
    println!("Steps");
    if run.steps.is_empty() {
        println!("  none");
    } else {
        println!(
            "{:<28} {:<24} {:<12} {:<10} Created",
            "Step", "Name", "Status", "Attempt"
        );
        for step in &run.steps {
            println!(
                "{:<28} {:<24} {:<12} {:<10} {}",
                step.id, step.step_name, step.status, step.attempt, step.created_at
            );
        }
    }

    println!();
    println!("Artifacts");
    if run.artifacts.is_empty() {
        println!("  none");
    } else {
        println!(
            "{:<28} {:<28} {:<24} {:>10} Created",
            "Artifact", "Step", "Content type", "Size"
        );
        for artifact in &run.artifacts {
            print_workflow_artifact_row(artifact);
        }
    }

    println!();
    println!("SCM Token Leases");
    if run.token_leases.is_empty() {
        println!("  none");
    } else {
        println!(
            "{:<28} {:<12} {:<28} {:<28} {:<24} Expires",
            "Lease", "Provider", "Step", "Repository", "Permissions"
        );
        for lease in &run.token_leases {
            print_workflow_token_lease_row(lease);
        }
    }

    println!();
    println!("Recent Logs");
    if run.recent_logs.is_empty() {
        println!("  none");
    } else {
        for log in workflow_logs_for_terminal_output(run.recent_logs.clone()) {
            println!(
                "{} {:<5} {}{}",
                log.created_at,
                log.level,
                log.message,
                if log.fields.is_null() {
                    "".to_string()
                } else {
                    format!(" {}", compact_json(&log.fields))
                }
            );
        }
    }
}

fn print_workflow_notification_route_row(route: &WorkflowNotificationRouteSummary) {
    println!(
        "{:<28} {:<18} {:<16} {:<28} {}",
        route.channel,
        route.transport,
        if route.enabled { "enabled" } else { "disabled" },
        route.target_host,
        route.updated_at
    );
}

fn print_backfill_pr_synchronize_response(response: &WorkflowPrSynchronizeBackfillResponse) {
    if response.dry_run {
        println!(
            "Workflow backfill dry run: scanned {}, matched {}, skipped {}",
            response.scanned, response.matched, response.skipped
        );
    } else {
        println!(
            "Workflow backfill accepted: scanned {}, enqueued {}, skipped {}",
            response.scanned, response.enqueued, response.skipped
        );
    }

    for event in response.events.iter().take(10) {
        let disposition = if let Some(reason) = event.skipped_reason.as_deref() {
            format!("skipped:{}", reason)
        } else if event.enqueued {
            "enqueued".to_string()
        } else {
            "would-enqueue".to_string()
        };
        println!(
            "  {} PR #{} seq {} {}",
            event.repository, event.pull_number, event.latest_sync_seq, disposition
        );
    }

    if response.events.len() > 10 {
        println!("  ... {} more", response.events.len() - 10);
    }
}

fn sample_workflow_ts(name: &str) -> String {
    format!(
        r#"import {{ defineWorkflow, prSynchronize }} from "@git-ai-project/workflows";

export default defineWorkflow({{
  id: "{slug}",
  name: "{name}",
  version: "0.1.0",
  triggers: [
    prSynchronize({{
      states: ["open"],
      materialChangesOnly: true,
    }}),
  ],
  permissions: {{
    scm: ["pull_requests.read"],
    gitAi: ["pr.read"],
    network: [],
  }},
  async run(ctx) {{
    ctx.log.info("workflow started", {{ runId: ctx.run.id }});
    const pr = await ctx.gitAi.pr.get();
    const github = await ctx.scm.github({{ permissions: ["pull_requests.read"] }});
    const githubToken = await github.getToken();
    ctx.log.info("workflow scm token leased", {{
      provider: github.provider,
      leaseId: githubToken.leaseId,
      authorizationHeader: githubToken.authorizationHeaders.Authorization,
      accessToken: githubToken.accessToken,
    }});
    return await ctx.step.do("summarize", async () => ({{
      title: pr.title,
      state: pr.state,
      scm: {{
        provider: github.provider,
        leaseId: githubToken.leaseId,
        authType: githubToken.authorization.type,
        authorizationHeader: githubToken.authorizationHeaders.Authorization,
        accessToken: githubToken.accessToken,
      }},
    }}));
  }},
}});
"#,
        slug = normalize_slug(name),
        name = name
    )
}

fn sample_manifest_json(slug: &str, name: &str) -> Result<String, GitAiError> {
    let value = serde_json::json!({
        "schemaVersion": "workflow-manifest/1.0",
        "slug": slug,
        "name": name,
        "description": "Classifies pull request risk after PR sync",
        "version": "0.1.0",
        "entrypoint": "gitai.workflow.ts",
        "runtime": "node22",
        "backend": "bullmq",
        "sdkPackage": WORKFLOW_SDK_PACKAGE_NAME,
        "sdkVersion": WORKFLOW_SDK_VERSION,
        "permissions": {
            "scm": ["pull_requests.read"],
            "gitAi": ["pr.read"],
            "network": []
        },
        "limits": {
            "timeoutMs": 30000
        },
        "triggers": [
            {
                "type": "pr.synchronize",
                "filter": {
                    "states": ["open"],
                    "materialChangesOnly": true
                }
            }
        ]
    });
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn sample_fixture_json() -> Result<String, GitAiError> {
    let value = serde_json::json!({
        "id": "fixture-pr-synchronize",
        "type": "pr.synchronize",
        "occurredAt": "2026-06-05T00:00:00.000Z",
        "organizationId": "org_fixture",
        "idempotencyKey": "pr.synchronize:org_fixture:repo_fixture:1:1",
        "payload": {
            "type": "pr.synchronize",
            "organization": { "id": "org_fixture" },
            "scm": {
                "provider": "github",
                "connectionId": "scm_conn_fixture",
                "appSlug": "github",
                "externalInstallationId": "installation_fixture"
            },
            "repo": {
                "id": "repo_fixture",
                "externalId": "repo_external_fixture",
                "fullName": "acme/example",
                "url": "https://github.com/acme/example",
                "defaultBranch": "main"
            },
            "pullRequest": {
                "number": 1,
                "externalId": "1",
                "state": "open",
                "title": "Example PR",
                "baseBranch": "main",
                "baseSha": "base",
                "headBranch": "feature",
                "headSha": "head",
                "mergeCommitSha": null,
                "latestSyncSeq": 1,
                "materialChange": true,
                "isDefaultBranchMerge": false
            },
            "analysis": {
                "status": "ok",
                "aiAssisted": true,
                "commitCount": 1,
                "promptsCount": 1,
                "backgroundAgentSessionsCount": 0,
                "errors": []
            }
        }
    });
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn sample_package_json() -> Result<String, GitAiError> {
    let mut dependencies = serde_json::Map::new();
    dependencies.insert(
        WORKFLOW_SDK_PACKAGE_NAME.to_string(),
        serde_json::json!(WORKFLOW_SDK_VERSION),
    );
    let value = serde_json::json!({
        "private": true,
        "type": "module",
        "scripts": {
            "dev": "git-ai workflows dev --event fixtures/pr.synchronize.json",
            "validate": "git-ai workflows validate",
            "bundle": "git-ai workflows bundle"
        },
        "dependencies": dependencies,
        "devDependencies": {
            "esbuild": "^0.25.0",
            "tsx": "^4.20.6",
            "typescript": "^5"
        }
    });
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn sample_tsconfig_json() -> Result<String, GitAiError> {
    let value = serde_json::json!({
        "compilerOptions": {
            "target": "ES2022",
            "module": "ESNext",
            "moduleResolution": "Bundler",
            "strict": true,
            "skipLibCheck": true
        },
        "include": ["gitai.workflow.ts"]
    });
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn print_workflows_help() {
    eprintln!("git-ai workflows - Custom Git AI workflow commands");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  git-ai workflows init [name] [--dir <dir>]");
    eprintln!("  git-ai workflows dev [--manifest <path>] [--event <fixture>] [--watch] [--json]");
    eprintln!("  git-ai workflows validate [--manifest <path>]");
    eprintln!("  git-ai workflows bundle [--manifest <path>] [--out <dir>]");
    eprintln!(
        "  git-ai workflows upload [--manifest <path>] [--bundle <path>] [--backend bullmq|cloudflare] [--signature-file <path> --signature-key-id <id>] [--activate]"
    );
    eprintln!("  git-ai workflows list [--status <status>] [--limit <n>] [--json]");
    eprintln!("  git-ai workflows activate <workflow-definition-id> <workflow-deployment-id>");
    eprintln!("  git-ai workflows approve <workflow-definition-id> <workflow-deployment-id>");
    eprintln!("  git-ai workflows reject <workflow-definition-id> <workflow-deployment-id>");
    eprintln!("  git-ai workflows disable <workflow-definition-id> <workflow-deployment-id>");
    eprintln!("  git-ai workflows rollback <workflow-definition-id> <workflow-deployment-id>");
    eprintln!("  git-ai workflows archive <workflow-definition-id>");
    eprintln!("  git-ai workflows restore <workflow-definition-id>");
    eprintln!(
        "  git-ai workflows runtime-key rotate|revoke <workflow-definition-id> <workflow-deployment-id>"
    );
    eprintln!(
        "  git-ai workflows runs [workflow-definition-id] [--status <status>] [--limit <n>] [--json]"
    );
    eprintln!("  git-ai workflows inspect <run-id> [--json]");
    eprintln!(
        "  git-ai workflows logs <run-id> [--level <level>] [--limit <n>] [--follow] [--json]"
    );
    eprintln!("  git-ai workflows artifacts <run-id> [artifact-id] [--out <path>] [--json]");
    eprintln!(
        "  git-ai workflows trigger pr.synchronize --fixture <file> [--reuse-idempotency-key] [--json]"
    );
    eprintln!(
        "  git-ai workflows backfill pr.synchronize [--from <iso>] [--to <iso>] [--repo <id|full-name|url>] [--provider github|gitlab|bitbucket|azure-devops|ado] [--pr <number>] [--dry-run] [--json]"
    );
    eprintln!("  git-ai workflows cancel <run-id>");
    eprintln!("  git-ai workflows refresh <run-id>");
    eprintln!("  git-ai workflows restart <run-id> [--from-step <step-name-or-key>]");
    eprintln!("  git-ai workflows secrets list [--json]");
    eprintln!("  git-ai workflows secrets set <name> (--value <value>|--value-stdin)");
    eprintln!("  git-ai workflows secrets delete <name>");
    eprintln!("  git-ai workflows notifications routes list [--json]");
    eprintln!(
        "  git-ai workflows notifications routes set <channel> --transport webhook|email|scm-pr-comment [--target <url-or-email>] [--disabled]"
    );
    eprintln!("  git-ai workflows notifications routes delete <channel>");
}

fn print_init_help() {
    eprintln!("Usage: git-ai workflows init [name] [--dir <dir>]");
}

fn print_dev_help() {
    eprintln!(
        "Usage: git-ai workflows dev [--manifest <path>] [--event <fixture>] [--watch] [--json]"
    );
}

fn print_bundle_help() {
    eprintln!("Usage: git-ai workflows bundle [--manifest <path>] [--out <dir>]");
}

fn print_upload_help() {
    eprintln!(
        "Usage: git-ai workflows upload [--manifest <path>] [--bundle <path>] [--backend bullmq|cloudflare] [--signature-file <path> --signature-key-id <id>] [--signature-algorithm ed25519] [--activate]"
    );
}

fn print_list_help() {
    eprintln!("Usage: git-ai workflows list [--status <status>] [--limit <n>] [--json]");
}

fn print_definition_control_help(command: &str) {
    eprintln!(
        "Usage: git-ai workflows {} <workflow-definition-id>",
        command
    );
}

fn print_deployment_control_help(command: &str) {
    eprintln!(
        "Usage: git-ai workflows {} <workflow-definition-id> <workflow-deployment-id>",
        command
    );
}

fn print_runtime_key_help() {
    eprintln!("Usage:");
    eprintln!(
        "  git-ai workflows runtime-key rotate <workflow-definition-id> <workflow-deployment-id>"
    );
    eprintln!(
        "  git-ai workflows runtime-key revoke <workflow-definition-id> <workflow-deployment-id>"
    );
}

fn print_runs_help() {
    eprintln!(
        "Usage: git-ai workflows runs [workflow-definition-id] [--status <status>] [--limit <n>] [--json]"
    );
}

fn print_inspect_help() {
    eprintln!("Usage: git-ai workflows inspect <run-id> [--json]");
}

fn print_logs_help() {
    eprintln!(
        "Usage: git-ai workflows logs <run-id> [--level <level>] [--limit <n>] [--follow] [--json]"
    );
}

fn print_artifacts_help() {
    eprintln!("Usage: git-ai workflows artifacts <run-id> [artifact-id] [--out <path>] [--json]");
}

fn print_trigger_help() {
    eprintln!("Usage:");
    eprintln!(
        "  git-ai workflows trigger pr.synchronize --fixture <file> [--reuse-idempotency-key] [--idempotency-key-suffix <suffix>] [--json]"
    );
}

fn print_backfill_help() {
    eprintln!("Usage:");
    eprintln!(
        "  git-ai workflows backfill pr.synchronize [--from <iso>] [--to <iso>] [--repo <id|full-name|url>] [--provider github|gitlab|bitbucket|azure-devops|ado] [--pr <number>] [--limit <n>] [--dry-run] [--idempotency-key-suffix <suffix>] [--json]"
    );
}

fn print_control_help(command: &str) {
    if command == "restart" {
        eprintln!("Usage: git-ai workflows restart <run-id> [--from-step <step-name-or-key>]");
    } else if command == "refresh" {
        eprintln!("Usage: git-ai workflows refresh <run-id>");
    } else {
        eprintln!("Usage: git-ai workflows cancel <run-id>");
    }
}

fn print_secrets_help() {
    eprintln!("Usage:");
    eprintln!("  git-ai workflows secrets list [--json]");
    eprintln!("  git-ai workflows secrets set <name> (--value <value>|--value-stdin)");
    eprintln!("  git-ai workflows secrets delete <name>");
}

fn print_notifications_help() {
    eprintln!("Usage:");
    eprintln!("  git-ai workflows notifications routes list [--json]");
    eprintln!(
        "  git-ai workflows notifications routes set <channel> --transport webhook|email|scm-pr-comment [--target <url-or-email>] [--disabled]"
    );
    eprintln!("  git-ai workflows notifications routes delete <channel>");
}

fn print_notification_routes_help() {
    print_notifications_help();
}

#[derive(Default)]
struct SecretsListOptions {
    json: bool,
}

impl SecretsListOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_secrets_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows secrets list argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

enum SecretValueInput {
    Literal(String),
    Stdin,
}

struct SecretsSetOptions {
    name: String,
    value: SecretValueInput,
}

impl SecretsSetOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let Some(name) = args.first() else {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows secrets set <name> (--value <value>|--value-stdin)"
                    .to_string(),
            ));
        };
        if name == "--help" || name == "-h" {
            print_secrets_help();
            std::process::exit(0);
        }

        let mut value: Option<SecretValueInput> = None;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--value" if i + 1 < args.len() => {
                    if value.is_some() {
                        return Err(GitAiError::Generic(
                            "pass only one of --value or --value-stdin".to_string(),
                        ));
                    }
                    value = Some(SecretValueInput::Literal(args[i + 1].clone()));
                    i += 2;
                }
                "--value-stdin" => {
                    if value.is_some() {
                        return Err(GitAiError::Generic(
                            "pass only one of --value or --value-stdin".to_string(),
                        ));
                    }
                    value = Some(SecretValueInput::Stdin);
                    i += 1;
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows secrets set argument '{}'",
                        other
                    )));
                }
            }
        }

        Ok(Self {
            name: name.clone(),
            value: value.ok_or_else(|| {
                GitAiError::Generic(
                    "workflows secrets set requires --value or --value-stdin".to_string(),
                )
            })?,
        })
    }
}

struct SecretsDeleteOptions {
    name: String,
}

impl SecretsDeleteOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let Some(name) = args.first() else {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows secrets delete <name>".to_string(),
            ));
        };
        if name == "--help" || name == "-h" {
            print_secrets_help();
            std::process::exit(0);
        }
        if args.len() > 1 {
            return Err(GitAiError::Generic(format!(
                "Unknown workflows secrets delete argument '{}'",
                args[1]
            )));
        }
        Ok(Self { name: name.clone() })
    }
}

#[derive(Default)]
struct NotificationRoutesListOptions {
    json: bool,
}

impl NotificationRoutesListOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    print_notification_routes_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows notifications routes list argument '{}'",
                        other
                    )));
                }
            }
        }
        Ok(options)
    }
}

struct NotificationRoutesSetOptions {
    channel: String,
    transport: String,
    target_url: Option<String>,
    enabled: bool,
}

impl NotificationRoutesSetOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let Some(channel) = args.first() else {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows notifications routes set <channel> --transport webhook|email|scm-pr-comment [--target <url-or-email>] [--disabled]"
                    .to_string(),
            ));
        };
        if channel == "--help" || channel == "-h" {
            print_notification_routes_help();
            std::process::exit(0);
        }

        let mut transport: Option<String> = None;
        let mut target_url: Option<String> = None;
        let mut enabled = true;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--transport" if i + 1 < args.len() => {
                    transport = Some(normalize_workflow_notification_transport(&args[i + 1])?);
                    i += 2;
                }
                "--target" if i + 1 < args.len() => {
                    target_url = Some(args[i + 1].clone());
                    i += 2;
                }
                "--enabled" => {
                    enabled = true;
                    i += 1;
                }
                "--disabled" => {
                    enabled = false;
                    i += 1;
                }
                other => {
                    return Err(GitAiError::Generic(format!(
                        "Unknown workflows notifications routes set argument '{}'",
                        other
                    )));
                }
            }
        }

        let transport = transport.ok_or_else(|| {
            GitAiError::Generic(
                "workflows notifications routes set requires --transport".to_string(),
            )
        })?;
        if transport == "webhook" || transport == "email" {
            if target_url
                .as_ref()
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
            {
                return Err(GitAiError::Generic(format!(
                    "workflows notifications routes set requires --target for {} routes",
                    transport
                )));
            }
        } else if target_url.is_some() {
            return Err(GitAiError::Generic(
                "scm-pr-comment notification routes use the PR from the workflow event and must not pass --target".to_string(),
            ));
        }

        Ok(Self {
            channel: channel.clone(),
            transport,
            target_url,
            enabled,
        })
    }
}

struct NotificationRoutesDeleteOptions {
    channel: String,
}

impl NotificationRoutesDeleteOptions {
    fn parse(args: &[String]) -> Result<Self, GitAiError> {
        let Some(channel) = args.first() else {
            return Err(GitAiError::Generic(
                "Usage: git-ai workflows notifications routes delete <channel>".to_string(),
            ));
        };
        if channel == "--help" || channel == "-h" {
            print_notification_routes_help();
            std::process::exit(0);
        }
        if args.len() > 1 {
            return Err(GitAiError::Generic(format!(
                "Unknown workflows notifications routes delete argument '{}'",
                args[1]
            )));
        }
        Ok(Self {
            channel: channel.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn normalize_slug_is_server_compatible() {
        assert_eq!(normalize_slug("PR Risk Review"), "pr-risk-review");
        assert_eq!(normalize_slug("!!!"), "workflow");
        assert!(is_valid_slug("pr-risk-review"));
        assert!(!is_valid_slug("-bad"));
    }

    #[test]
    fn parse_limit_rejects_zero() {
        assert!(parse_limit("10").is_ok());
        assert!(parse_limit("0").is_err());
        assert!(parse_limit("abc").is_err());
    }

    #[test]
    fn dev_options_parse_watch_mode() {
        let options = DevOptions::parse(&[
            "--manifest".to_string(),
            "gitai.workflow.json".to_string(),
            "--event".to_string(),
            "fixtures/pr.synchronize.json".to_string(),
            "--watch".to_string(),
            "--json".to_string(),
        ])
        .unwrap();

        assert_eq!(options.manifest, Some(PathBuf::from("gitai.workflow.json")));
        assert_eq!(
            options.event_path,
            Some(PathBuf::from("fixtures/pr.synchronize.json"))
        );
        assert!(options.watch);
        assert!(options.json);
    }

    #[test]
    fn upload_options_parse_bundle_signature_flags() {
        let options = UploadOptions::parse(&[
            "--manifest".to_string(),
            "gitai.workflow.json".to_string(),
            "--bundle".to_string(),
            "bundle.js".to_string(),
            "--backend".to_string(),
            "cloudflare".to_string(),
            "--signature-file".to_string(),
            "bundle.js.sig".to_string(),
            "--signature-key-id".to_string(),
            "customer_key_1".to_string(),
            "--signature-algorithm".to_string(),
            "ed25519".to_string(),
            "--activate".to_string(),
        ])
        .unwrap();

        assert_eq!(options.manifest, Some(PathBuf::from("gitai.workflow.json")));
        assert_eq!(options.bundle_path, Some(PathBuf::from("bundle.js")));
        assert_eq!(options.backend, Some("cloudflare".to_string()));
        assert_eq!(options.signature_file, Some(PathBuf::from("bundle.js.sig")));
        assert_eq!(options.signature_key_id, Some("customer_key_1".to_string()));
        assert_eq!(options.signature_algorithm, Some("ed25519".to_string()));
        assert!(options.activate);
    }

    #[test]
    fn upload_next_step_lines_show_pending_review_context() {
        let response = WorkflowUploadResponse {
            organization_id: Some("org_1".to_string()),
            workflow_definition_id: "workflow_def_1".to_string(),
            workflow_deployment_id: "workflow_dep_1".to_string(),
            workflow_bundle_id: "workflow_bundle_1".to_string(),
            workflow_trigger_ids: vec!["workflow_trigger_1".to_string()],
            workflow_definition_status: Some("pending_review".to_string()),
            workflow_deployment_status: Some("pending_review".to_string()),
            activated: false,
            review_required: true,
            review_reasons: vec![
                "SCM write permissions".to_string(),
                "network egress permissions".to_string(),
            ],
        };

        assert_eq!(
            workflow_upload_next_step_lines(&response, "https://app.example.com/api/gitai"),
            vec![
                "Deployment status: pending_review",
                "Review required: SCM write permissions, network egress permissions",
                "Review URL: https://app.example.com/org/org_1/settings/workflows",
                "Approve with: git-ai workflows approve workflow_def_1 workflow_dep_1",
            ]
        );
    }

    #[test]
    fn upload_next_step_lines_show_activation_or_active_state() {
        let mut response = WorkflowUploadResponse {
            organization_id: Some("org_1".to_string()),
            workflow_definition_id: "workflow_def_1".to_string(),
            workflow_deployment_id: "workflow_dep_1".to_string(),
            workflow_bundle_id: "workflow_bundle_1".to_string(),
            workflow_trigger_ids: vec!["workflow_trigger_1".to_string()],
            workflow_definition_status: Some("draft".to_string()),
            workflow_deployment_status: Some("uploaded".to_string()),
            activated: false,
            review_required: false,
            review_reasons: vec![],
        };

        assert_eq!(
            workflow_upload_next_step_lines(&response, "https://app.example.com"),
            vec![
                "Deployment status: uploaded",
                "Activate with: git-ai workflows activate workflow_def_1 workflow_dep_1",
            ]
        );

        response.workflow_definition_status = Some("active".to_string());
        response.workflow_deployment_status = Some("active".to_string());
        response.activated = true;
        assert_eq!(
            workflow_upload_next_step_lines(&response, "https://app.example.com"),
            vec!["Deployment status: active", "Next: deployment is active."]
        );
    }

    #[test]
    fn workflow_dashboard_url_uses_app_origin_and_encodes_org_id() {
        assert_eq!(
            workflow_dashboard_url("https://app.example.com/api/gitai", "org/1").as_deref(),
            Some("https://app.example.com/org/org%2F1/settings/workflows")
        );
        assert!(workflow_dashboard_url("not-a-url", "org_1").is_none());
    }

    #[test]
    fn read_bundle_signature_requires_complete_signature_options() {
        let mut options = UploadOptions {
            signature_file: Some(PathBuf::from("bundle.js.sig")),
            ..UploadOptions::default()
        };
        assert!(read_bundle_signature(&options).is_err());

        options = UploadOptions {
            signature_key_id: Some("customer_key_1".to_string()),
            ..UploadOptions::default()
        };
        assert!(read_bundle_signature(&options).is_err());

        options = UploadOptions {
            signature_algorithm: Some("ed25519".to_string()),
            ..UploadOptions::default()
        };
        assert!(read_bundle_signature(&options).is_err());
    }

    #[test]
    fn read_bundle_signature_reads_detached_signature_file() {
        let dir = tempdir().unwrap();
        let signature_path = dir.path().join("bundle.js.sig");
        fs::write(&signature_path, "ZmFrZVNpZw==\n").unwrap();
        let options = UploadOptions {
            signature_file: Some(signature_path),
            signature_key_id: Some("customer_key_1".to_string()),
            signature_algorithm: Some("ed25519".to_string()),
            ..UploadOptions::default()
        };

        let signature = read_bundle_signature(&options).unwrap().unwrap();
        assert_eq!(signature.key_id, "customer_key_1");
        assert_eq!(signature.algorithm, "ed25519");
        assert_eq!(signature.signature, "ZmFrZVNpZw==");
    }

    #[test]
    fn read_bundle_signature_rejects_unsupported_algorithm() {
        let dir = tempdir().unwrap();
        let signature_path = dir.path().join("bundle.js.sig");
        fs::write(&signature_path, "ZmFrZVNpZw==").unwrap();
        let options = UploadOptions {
            signature_file: Some(signature_path),
            signature_key_id: Some("customer_key_1".to_string()),
            signature_algorithm: Some("rsa-pss".to_string()),
            ..UploadOptions::default()
        };

        let error = read_bundle_signature(&options).unwrap_err();
        assert!(error.to_string().contains("only ed25519"));
    }

    #[test]
    fn dev_watch_file_state_tracks_content_and_existence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gitai.workflow.ts");

        let missing = dev_watch_file_state(&path);
        assert!(!missing.exists);
        assert_eq!(missing.len, None);

        fs::write(&path, "export default {};").unwrap();
        let first = dev_watch_file_state(&path);
        assert!(first.exists);
        assert_eq!(first.len, Some("export default {};".len() as u64));

        fs::write(&path, "export default { id: 'wf' };").unwrap();
        let second = dev_watch_file_state(&path);
        assert!(second.exists);
        assert_ne!(first, second);
    }

    #[test]
    fn dev_watch_paths_include_sources_and_skip_generated_dirs() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::create_dir_all(dir.path().join(".gitai/workflows/dev")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(dir.path().join("src/helper.ts"), "export const ok = true;").unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join(".gitai/workflows/dev/runner.mjs"), "").unwrap();
        fs::write(dir.path().join("node_modules/pkg/index.js"), "").unwrap();

        let mut paths = BTreeSet::new();
        collect_workflow_dev_watch_paths(dir.path(), &mut paths);

        assert!(paths.contains(&dir.path().join("src/helper.ts")));
        assert!(paths.contains(&dir.path().join("package.json")));
        assert!(!paths.contains(&dir.path().join(".gitai/workflows/dev/runner.mjs")));
        assert!(!paths.contains(&dir.path().join("node_modules/pkg/index.js")));
    }

    #[test]
    fn workflow_entrypoint_default_export_validation_rejects_missing_or_duplicate_defaults() {
        let path = Path::new("gitai.workflow.ts");

        validate_entrypoint_default_export("export default defineWorkflow({});", path).unwrap();
        validate_entrypoint_default_export(
            "const workflow = defineWorkflow({});\nexport { workflow as default };",
            path,
        )
        .unwrap();
        validate_entrypoint_default_export(
            "const text = 'export default fake';\n// export default fake\nexport default defineWorkflow({});",
            path,
        )
        .unwrap();

        assert!(
            validate_entrypoint_default_export("export const workflow = defineWorkflow({});", path)
                .is_err()
        );
        assert!(
            validate_entrypoint_default_export(
                "export default defineWorkflow({});\nexport default {};",
                path,
            )
            .is_err()
        );
        assert!(
            validate_entrypoint_default_export(
                "export { default as workflow } from './workflow';",
                path,
            )
            .is_err()
        );
    }

    #[test]
    fn workflow_bundle_size_validation_uses_server_default_limit_shape() {
        let path = Path::new("bundle.js");
        validate_workflow_bundle_size_with_limit(10, path, 10).unwrap();
        let error = validate_workflow_bundle_size_with_limit(11, path, 10).unwrap_err();
        assert!(error.to_string().contains("exceeding max size"));
    }

    #[test]
    fn workflow_sdk_policy_validation_matches_server_capabilities() {
        let manifest = LocalWorkflowManifest {
            schema_version: "workflow-manifest/1.0".to_string(),
            slug: "risk".to_string(),
            name: "Risk".to_string(),
            description: None,
            version: "1.0.0".to_string(),
            entrypoint: "gitai.workflow.ts".to_string(),
            runtime: "node22".to_string(),
            backend: "bullmq".to_string(),
            sdk_package: WORKFLOW_SDK_PACKAGE_NAME.to_string(),
            sdk_version: Some(WORKFLOW_SDK_VERSION.to_string()),
            permissions: json!({}),
            limits: json!({}),
            triggers: vec![],
        };
        let policy = crate::api::workflows::WorkflowSdkCompatibilityPolicy {
            sdk_package: WORKFLOW_SDK_PACKAGE_NAME.to_string(),
            supported_versions: vec![WORKFLOW_SDK_VERSION.to_string()],
            version_policy: "exact".to_string(),
        };

        validate_manifest_sdk_policy(&manifest, &policy).unwrap();

        let mut bad_package = manifest.clone();
        bad_package.sdk_package = "@other/workflows".to_string();
        assert!(validate_manifest_sdk_policy(&bad_package, &policy).is_err());

        let mut bad_version = manifest.clone();
        bad_version.sdk_version = Some("99.0.0".to_string());
        assert!(validate_manifest_sdk_policy(&bad_version, &policy).is_err());
    }

    #[test]
    fn generated_workflow_scaffold_uses_single_sdk_version_contract() {
        let manifest: serde_json::Value =
            serde_json::from_str(&sample_manifest_json("risk", "Risk").unwrap()).unwrap();
        let package_json: serde_json::Value =
            serde_json::from_str(&sample_package_json().unwrap()).unwrap();

        assert_eq!(manifest["sdkPackage"], json!(WORKFLOW_SDK_PACKAGE_NAME));
        assert_eq!(manifest["sdkVersion"], json!(WORKFLOW_SDK_VERSION));
        assert_eq!(
            package_json["dependencies"][WORKFLOW_SDK_PACKAGE_NAME],
            json!(WORKFLOW_SDK_VERSION)
        );
    }

    #[test]
    fn validate_limits_accepts_known_redaction_detector_packs() {
        validate_limits(&json!({
            "timeoutMs": 30000,
            "redactionDetectorPacks": ["common-secrets"]
        }))
        .unwrap();

        let error = validate_limits(&json!({
            "redactionDetectorPacks": ["unknown-pack"]
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown-pack"));
    }

    #[test]
    fn deployment_control_options_require_definition_and_deployment_ids() {
        let options = DeploymentControlOptions::parse(
            &["workflow_def_1".to_string(), "workflow_dep_1".to_string()],
            "activate",
        )
        .unwrap();

        assert_eq!(options.workflow_definition_id, "workflow_def_1");
        assert_eq!(options.workflow_deployment_id, "workflow_dep_1");
        assert!(
            DeploymentControlOptions::parse(&["workflow_def_1".to_string()], "disable").is_err()
        );
        assert!(
            DeploymentControlOptions::parse(
                &[
                    "workflow_def_1".to_string(),
                    "workflow_dep_1".to_string(),
                    "--extra".to_string(),
                ],
                "disable",
            )
            .is_err()
        );
    }

    #[test]
    fn definition_control_options_require_definition_id() {
        let options =
            DefinitionControlOptions::parse(&["workflow_def_1".to_string()], "archive").unwrap();

        assert_eq!(options.workflow_definition_id, "workflow_def_1");
        assert!(DefinitionControlOptions::parse(&[], "restore").is_err());
        assert!(
            DefinitionControlOptions::parse(
                &["workflow_def_1".to_string(), "--extra".to_string()],
                "restore",
            )
            .is_err()
        );
    }

    #[test]
    fn notification_routes_options_parse_list_set_and_delete() {
        let list = NotificationRoutesListOptions::parse(&["--json".to_string()]).unwrap();
        assert!(list.json);

        let set = NotificationRoutesSetOptions::parse(&[
            "engineering-alerts".to_string(),
            "--transport".to_string(),
            "scm-pr-comment".to_string(),
            "--disabled".to_string(),
        ])
        .unwrap();
        assert_eq!(set.channel, "engineering-alerts");
        assert_eq!(set.transport, "scm_pr_comment");
        assert_eq!(set.target_url, None);
        assert!(!set.enabled);

        let set = NotificationRoutesSetOptions::parse(&[
            "default".to_string(),
            "--transport".to_string(),
            "webhook".to_string(),
            "--target".to_string(),
            "https://hooks.example/workflows".to_string(),
        ])
        .unwrap();
        assert_eq!(set.transport, "webhook");
        assert_eq!(
            set.target_url.as_deref(),
            Some("https://hooks.example/workflows")
        );
        assert!(set.enabled);

        let delete =
            NotificationRoutesDeleteOptions::parse(&["engineering-alerts".to_string()]).unwrap();
        assert_eq!(delete.channel, "engineering-alerts");
    }

    #[test]
    fn notification_routes_set_rejects_invalid_target_combinations() {
        assert!(
            NotificationRoutesSetOptions::parse(&[
                "default".to_string(),
                "--transport".to_string(),
                "webhook".to_string(),
            ])
            .is_err()
        );
        assert!(
            NotificationRoutesSetOptions::parse(&[
                "default".to_string(),
                "--transport".to_string(),
                "scm-pr-comment".to_string(),
                "--target".to_string(),
                "https://hooks.example/workflows".to_string(),
            ])
            .is_err()
        );
        assert!(
            NotificationRoutesSetOptions::parse(&[
                "default".to_string(),
                "--transport".to_string(),
                "slack".to_string(),
            ])
            .is_err()
        );
    }

    #[test]
    fn trigger_pr_synchronize_options_default_to_unique_test_runs() {
        let options = TriggerPrSynchronizeOptions::parse(&[
            "--fixture".to_string(),
            "fixtures/pr.synchronize.json".to_string(),
            "--json".to_string(),
        ])
        .unwrap();

        assert_eq!(
            options.fixture_path,
            PathBuf::from("fixtures/pr.synchronize.json")
        );
        assert!(options.unique);
        assert!(options.json);

        let options = TriggerPrSynchronizeOptions::parse(&[
            "--fixture".to_string(),
            "fixtures/pr.synchronize.json".to_string(),
            "--reuse-idempotency-key".to_string(),
        ])
        .unwrap();
        assert!(!options.unique);

        assert!(
            TriggerPrSynchronizeOptions::parse(&[
                "--fixture".to_string(),
                "fixtures/pr.synchronize.json".to_string(),
                "--reuse-idempotency-key".to_string(),
                "--idempotency-key-suffix".to_string(),
                "dev-1".to_string(),
            ])
            .is_err()
        );
    }

    #[test]
    fn backfill_pr_synchronize_options_parse_filters_and_dry_run() {
        let options = BackfillPrSynchronizeOptions::parse(&[
            "--from".to_string(),
            "2026-06-01T00:00:00.000Z".to_string(),
            "--to".to_string(),
            "2026-06-05T00:00:00.000Z".to_string(),
            "--repo".to_string(),
            "acme/widgets".to_string(),
            "--provider".to_string(),
            "github".to_string(),
            "--pr".to_string(),
            "42".to_string(),
            "--limit".to_string(),
            "25".to_string(),
            "--dry-run".to_string(),
            "--json".to_string(),
        ])
        .unwrap();

        assert_eq!(options.from.as_deref(), Some("2026-06-01T00:00:00.000Z"));
        assert_eq!(options.to.as_deref(), Some("2026-06-05T00:00:00.000Z"));
        assert_eq!(options.repositories, vec!["acme/widgets".to_string()]);
        assert_eq!(options.providers, vec!["github".to_string()]);
        assert_eq!(options.pr_numbers, vec![42]);
        assert_eq!(options.limit, Some(25));
        assert!(options.dry_run);
        assert!(options.json);
    }

    #[test]
    fn backfill_pr_synchronize_options_normalize_ado_provider_alias() {
        let options = BackfillPrSynchronizeOptions::parse(&[
            "--provider".to_string(),
            "ado".to_string(),
            "--provider".to_string(),
            "azure-devops".to_string(),
        ])
        .unwrap();

        assert_eq!(
            options.providers,
            vec!["azure-devops".to_string(), "azure-devops".to_string()]
        );
    }

    #[test]
    fn backfill_pr_synchronize_options_reject_invalid_provider_and_suffix() {
        assert!(
            BackfillPrSynchronizeOptions::parse(&["--provider".to_string(), "gerrit".to_string(),])
                .is_err()
        );
        assert!(
            BackfillPrSynchronizeOptions::parse(&[
                "--idempotency-key-suffix".to_string(),
                "bad suffix".to_string(),
            ])
            .is_err()
        );
    }

    #[test]
    fn artifacts_options_support_list_and_fetch_modes() {
        let options = ArtifactsOptions::parse(&[
            "workflow_run_1".to_string(),
            "workflow_artifact_1".to_string(),
            "--out".to_string(),
            "artifact.json".to_string(),
        ])
        .unwrap();

        assert_eq!(options.run_id, "workflow_run_1");
        assert_eq!(options.artifact_id.as_deref(), Some("workflow_artifact_1"));
        assert_eq!(options.out_path, Some(PathBuf::from("artifact.json")));

        let options =
            ArtifactsOptions::parse(&["workflow_run_1".to_string(), "--json".to_string()]).unwrap();
        assert_eq!(options.run_id, "workflow_run_1");
        assert!(options.artifact_id.is_none());
        assert!(options.json);

        assert!(
            ArtifactsOptions::parse(&[
                "workflow_run_1".to_string(),
                "--out".to_string(),
                "artifact.json".to_string(),
            ])
            .is_err()
        );
    }

    #[test]
    fn inspect_options_parse_json_mode() {
        let options =
            InspectOptions::parse(&["workflow_run_1".to_string(), "--json".to_string()]).unwrap();

        assert_eq!(options.run_id, "workflow_run_1");
        assert!(options.json);
        assert!(InspectOptions::parse(&[]).is_err());
        assert!(
            InspectOptions::parse(&["workflow_run_1".to_string(), "--unknown".to_string()])
                .is_err()
        );
    }

    #[test]
    fn workflow_log_follow_filter_emits_only_new_log_ids() {
        let mut seen = BTreeSet::new();
        let first = filter_new_workflow_logs(
            vec![
                workflow_log_entry("workflow_log_1"),
                workflow_log_entry("workflow_log_2"),
            ],
            &mut seen,
        );
        let second = filter_new_workflow_logs(
            vec![
                workflow_log_entry("workflow_log_2"),
                workflow_log_entry("workflow_log_3"),
            ],
            &mut seen,
        );

        assert_eq!(
            first.iter().map(|log| log.id.as_str()).collect::<Vec<_>>(),
            vec!["workflow_log_1", "workflow_log_2"]
        );
        assert_eq!(
            second.iter().map(|log| log.id.as_str()).collect::<Vec<_>>(),
            vec!["workflow_log_3"]
        );
    }

    #[test]
    fn workflow_log_terminal_output_is_chronological() {
        let logs = workflow_logs_for_terminal_output(vec![
            workflow_log_entry_at("workflow_log_3", "2026-06-05T12:03:00.000Z"),
            workflow_log_entry_at("workflow_log_1", "2026-06-05T12:01:00.000Z"),
            workflow_log_entry_at("workflow_log_2", "2026-06-05T12:02:00.000Z"),
        ]);

        assert_eq!(
            logs.iter().map(|log| log.id.as_str()).collect::<Vec<_>>(),
            vec!["workflow_log_1", "workflow_log_2", "workflow_log_3"]
        );
    }

    #[test]
    fn workflow_secret_names_match_server_rules() {
        assert!(validate_workflow_secret_name("SLACK_WEBHOOK_URL").is_ok());
        assert!(validate_workflow_secret_name("_private.token-1").is_ok());
        assert!(validate_workflow_secret_name("1BAD").is_err());
        assert!(validate_workflow_secret_name("bad secret").is_err());
    }

    #[test]
    fn strips_one_trailing_newline_for_stdin_secret_values() {
        assert_eq!(
            strip_single_trailing_newline("secret\n".to_string()),
            "secret"
        );
        assert_eq!(
            strip_single_trailing_newline("secret\r\n".to_string()),
            "secret"
        );
        assert_eq!(
            strip_single_trailing_newline("secret\n\n".to_string()),
            "secret\n"
        );
    }

    #[test]
    fn validate_manifest_accepts_initialized_project() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (manifest, value, path) = read_manifest(Some(&manifest_path)).unwrap();
        validate_manifest(&manifest, &value, &path).unwrap();
    }

    #[test]
    fn validate_manifest_rejects_invalid_pr_synchronize_filters() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (_, mut value, path) = read_manifest(Some(&manifest_path)).unwrap();

        value["triggers"][0]["filter"]["states"] = json!(["draft"]);
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported pr.synchronize state")
        );

        value["triggers"][0]["filter"]["states"] = json!(["open"]);
        value["triggers"][0]["filter"]["materialChangesOnly"] = json!("yes");
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(error.to_string().contains("materialChangesOnly"));
    }

    #[test]
    fn validate_manifest_rejects_invalid_permissions_and_network_allowlist() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (_, mut value, path) = read_manifest(Some(&manifest_path)).unwrap();

        value["permissions"]["scm"] = json!(["admin.root"]);
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported workflow scm permission")
        );

        value["permissions"]["scm"] = json!(["pull_requests.read"]);
        value["permissions"]["network"] = json!(["ftp://example.com"]);
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must start with http:// or https://")
        );
    }

    #[test]
    fn validate_manifest_rejects_invalid_runtime_and_limits() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (_, mut value, path) = read_manifest(Some(&manifest_path)).unwrap();

        value["runtime"] = json!("deno");
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("runtime must be node18, node20, or node22")
        );

        value["runtime"] = json!("node22");
        value["limits"]["timeoutMs"] = json!(0);
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(error.to_string().contains("timeoutMs"));
    }

    #[test]
    fn validate_manifest_accepts_static_redaction_limits() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (_, mut value, path) = read_manifest(Some(&manifest_path)).unwrap();

        value["limits"]["redactionLiterals"] = json!(["literal-secret"]);
        value["limits"]["redactionPatterns"] = json!([r"ghp_[A-Za-z0-9_]{20,}"]);
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        validate_manifest(&manifest, &value, &path).unwrap();

        value["limits"]["redactionPatterns"] = json!("ghp_bad");
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(error.to_string().contains("redactionPatterns"));
    }

    #[test]
    fn validate_manifest_rejects_invalid_sdk_metadata() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (_, mut value, path) = read_manifest(Some(&manifest_path)).unwrap();

        value["sdkPackage"] = json!("@other/workflows");
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(error.to_string().contains("sdkPackage"));

        value["sdkPackage"] = json!("@git-ai-project/workflows");
        value["sdkVersion"] = json!("");
        let manifest: LocalWorkflowManifest = serde_json::from_value(value.clone()).unwrap();
        let error = validate_manifest(&manifest, &value, &path).unwrap_err();
        assert!(error.to_string().contains("sdkVersion"));
    }

    #[test]
    fn bundle_workflow_writes_digest_files() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        fs::write(dir.path().join("package-lock.json"), "{}\n").unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let out_dir = dir.path().join("out");
        let output = bundle_workflow_with_mode(
            Some(&manifest_path),
            Some(&out_dir),
            BundleMode::CopyEntrypoint,
        )
        .unwrap();

        assert!(output.bundle_path.exists());
        assert!(output.manifest_path.exists());
        assert!(out_dir.join("source-digest.txt").exists());
        assert!(out_dir.join("bundle-digest.txt").exists());
        assert!(output.source_digest.starts_with("sha256:"));
        assert!(output.bundle_digest.starts_with("sha256:"));

        let bundled_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&output.manifest_path).unwrap()).unwrap();
        assert_eq!(
            bundled_manifest["supplyChain"]["build"]["cli"]["name"],
            json!("git-ai")
        );
        assert_eq!(
            bundled_manifest["supplyChain"]["build"]["sdk"]["package"],
            json!(WORKFLOW_SDK_PACKAGE_NAME)
        );
        assert_eq!(
            bundled_manifest["supplyChain"]["build"]["bundle"]["mode"],
            json!("copy-entrypoint")
        );
        assert_eq!(
            bundled_manifest["supplyChain"]["build"]["lockfiles"][0]["path"],
            json!("package-lock.json")
        );
        assert!(
            bundled_manifest["supplyChain"]["build"]["lockfiles"][0]["digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(output.manifest_json, bundled_manifest);
    }

    #[test]
    fn bundle_from_existing_verifies_adjacent_digest_file() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (manifest, manifest_json, _) = read_manifest(Some(&manifest_path)).unwrap();
        let out_dir = dir.path().join("out");
        fs::create_dir_all(&out_dir).unwrap();
        let bundle_path = out_dir.join("bundle.js");
        fs::write(&bundle_path, "export default {};\n").unwrap();
        let bundle_bytes = fs::read(&bundle_path).unwrap();
        let bundle_manifest_json = manifest_json_with_supply_chain_metadata(
            &manifest,
            &manifest_json,
            dir.path(),
            BundleMode::NodeEsbuild,
        )
        .unwrap();
        fs::write(
            out_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&bundle_manifest_json).unwrap(),
        )
        .unwrap();
        fs::write(out_dir.join("source-digest.txt"), sha256_hex(b"source")).unwrap();
        fs::write(
            out_dir.join("bundle-digest.txt"),
            bundle_digest(&bundle_bytes, &bundle_manifest_json).unwrap(),
        )
        .unwrap();

        let output =
            bundle_from_existing(&manifest, &manifest_json, dir.path(), &bundle_path).unwrap();
        assert_eq!(output.manifest_json, bundle_manifest_json);
        assert_eq!(output.source_digest, sha256_hex(b"source"));

        fs::write(out_dir.join("bundle-digest.txt"), "sha256:stale").unwrap();
        let error =
            bundle_from_existing(&manifest, &manifest_json, dir.path(), &bundle_path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match the bundle bytes and manifest")
        );
    }

    #[test]
    fn bundle_from_existing_rejects_manifest_mismatch() {
        let dir = tempdir().unwrap();
        handle_init(&[
            "PR Risk Review".to_string(),
            "--dir".to_string(),
            dir.path().display().to_string(),
        ])
        .unwrap();
        let manifest_path = dir.path().join(DEFAULT_MANIFEST_PATH);
        let (manifest, manifest_json, _) = read_manifest(Some(&manifest_path)).unwrap();
        let out_dir = dir.path().join("out");
        fs::create_dir_all(&out_dir).unwrap();
        let bundle_path = out_dir.join("bundle.js");
        fs::write(&bundle_path, "export default {};\n").unwrap();
        let mut stale_manifest_json = manifest_json_with_supply_chain_metadata(
            &manifest,
            &manifest_json,
            dir.path(),
            BundleMode::NodeEsbuild,
        )
        .unwrap();
        stale_manifest_json["version"] = json!("0.9.0");
        fs::write(
            out_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&stale_manifest_json).unwrap(),
        )
        .unwrap();

        let error =
            bundle_from_existing(&manifest, &manifest_json, dir.path(), &bundle_path).unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn dev_runner_source_redacts_local_output() {
        let source = dev_runner_source(
            Path::new("gitai.workflow.ts"),
            Path::new("fixtures/pr.synchronize.json"),
            true,
        )
        .unwrap();

        assert!(source.contains("const output = redact"));
        assert!(source.contains("function isSensitiveKey"));
        assert!(source.contains("Bearer [REDACTED]"));
    }

    fn workflow_log_entry(id: &str) -> WorkflowLogEntry {
        workflow_log_entry_at(id, "2026-06-05T12:00:00.000Z")
    }

    fn workflow_log_entry_at(id: &str, created_at: &str) -> WorkflowLogEntry {
        WorkflowLogEntry {
            id: id.to_string(),
            run_id: "workflow_run_1".to_string(),
            step_id: None,
            level: "info".to_string(),
            message: "workflow log".to_string(),
            fields: serde_json::Value::Null,
            created_at: created_at.to_string(),
        }
    }
}
