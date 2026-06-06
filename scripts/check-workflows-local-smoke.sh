#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: scripts/check-workflows-local-smoke.sh [--sdk-root <path>] [--keep-tmp]

Runs the generated workflow scaffold against a local @git-ai-project/workflows
package source tree, then uploads the generated bundle through a local fake
Git AI workflow API. All npm installs and package build output are kept in a
temporary directory.

Options:
  --sdk-root <path>  Path to the @git-ai-project/workflows package source.
                    Defaults to GIT_AI_WORKFLOWS_SDK_ROOT, or the only matching
                    package under ../monorepo/packages/workflows or
                    ../monorepo/.worktrees/*/*/packages/workflows.
  --keep-tmp        Leave the temporary smoke directory in place.
EOF
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cli_root="$(cd "$script_dir/.." && pwd)"
sdk_root="${GIT_AI_WORKFLOWS_SDK_ROOT:-}"
keep_tmp=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --sdk-root)
      if [ "$#" -lt 2 ]; then
        echo "Error: --sdk-root requires a path" >&2
        usage
        exit 2
      fi
      sdk_root="$2"
      shift 2
      ;;
    --keep-tmp)
      keep_tmp=true
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Error: unknown argument '$1'" >&2
      usage
      exit 2
      ;;
  esac
done

is_workflows_sdk_root() {
  local candidate="$1"
  [ -f "$candidate/package.json" ] &&
    grep -q '"name"[[:space:]]*:[[:space:]]*"@git-ai-project/workflows"' "$candidate/package.json"
}

if [ -z "$sdk_root" ]; then
  shopt -s nullglob
  candidates=(
    "$cli_root/../monorepo/packages/workflows"
    "$cli_root/../monorepo/.worktrees"/*/*/packages/workflows
  )
  shopt -u nullglob

  matches=()
  for candidate in "${candidates[@]}"; do
    if is_workflows_sdk_root "$candidate"; then
      matches+=("$candidate")
    fi
  done

  if [ "${#matches[@]}" -eq 1 ]; then
    sdk_root="${matches[0]}"
  elif [ "${#matches[@]}" -eq 0 ]; then
    echo "Error: could not find @git-ai-project/workflows; pass --sdk-root or set GIT_AI_WORKFLOWS_SDK_ROOT" >&2
    exit 2
  else
    echo "Error: found multiple @git-ai-project/workflows package roots; pass --sdk-root explicitly" >&2
    printf '  %s\n' "${matches[@]}" >&2
    exit 2
  fi
fi

if ! is_workflows_sdk_root "$sdk_root"; then
  echo "Error: '$sdk_root' is not an @git-ai-project/workflows package root" >&2
  exit 2
fi

sdk_root="$(cd "$sdk_root" && pwd)"

for required in cargo node npm; do
  if ! command -v "$required" >/dev/null 2>&1; then
    echo "Error: '$required' is required for the workflows local smoke" >&2
    exit 2
  fi
done

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/git-ai-workflow-smoke.XXXXXX")"
mock_api_pid=""
cleanup() {
  if [ -n "$mock_api_pid" ]; then
    kill "$mock_api_pid" 2>/dev/null || true
    wait "$mock_api_pid" 2>/dev/null || true
  fi
  if [ "$keep_tmp" = true ]; then
    echo "Kept smoke directory: $tmpdir"
  else
    rm -rf "$tmpdir"
  fi
}
trap cleanup EXIT

project_dir="$tmpdir/project"
sdk_copy="$tmpdir/workflows-sdk"
bundle_dir="$tmpdir/bundle"
mock_api_port_file="$tmpdir/mock-api-port"
mock_api_log="$tmpdir/mock-api-requests.jsonl"
mock_home="$tmpdir/home"

start_mock_api() {
  rm -f "$mock_api_port_file" "$mock_api_log"
  node - "$mock_api_port_file" "$mock_api_log" <<'NODE' &
const fs = require("node:fs");
const http = require("node:http");
const { URL } = require("node:url");

const [, , portFile, logFile] = process.argv;
const expectedApiKey = "workflow-smoke-api-key";

function sendJson(res, status, body) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
}

function readJson(req) {
  return new Promise((resolve, reject) => {
    let raw = "";
    req.on("data", (chunk) => {
      raw += chunk;
      if (raw.length > 20 * 1024 * 1024) {
        reject(new Error("request body too large"));
        req.destroy();
      }
    });
    req.on("end", () => {
      try {
        resolve(raw ? JSON.parse(raw) : {});
      } catch (error) {
        reject(error);
      }
    });
    req.on("error", reject);
  });
}

function rejectUpload(res, message) {
  sendJson(res, 400, { error: message });
}

function requireApiKey(req, res) {
  if (req.headers["x-api-key"] !== expectedApiKey) {
    sendJson(res, 401, { error: "missing expected X-API-Key header" });
    return false;
  }
  return true;
}

function validateUpload(req, body) {
  if (req.headers["x-api-key"] !== expectedApiKey) {
    return "missing expected X-API-Key header";
  }
  if (body?.definition?.slug !== "pr-risk-review") {
    return "missing workflow definition slug";
  }
  if (body?.deployment?.backend !== "bullmq") {
    return "upload did not select bullmq backend";
  }
  if (!String(body?.deployment?.bundleDigest || "").startsWith("sha256:")) {
    return "missing bundle digest";
  }
  if (!String(body?.deployment?.sourceDigest || "").startsWith("sha256:")) {
    return "missing source digest";
  }
  if (!body?.bundle?.contentBase64) {
    return "missing inline bundle content";
  }
  if (Buffer.from(body.bundle.contentBase64, "base64").length === 0) {
    return "empty inline bundle content";
  }
  const triggers = Array.isArray(body?.triggers) ? body.triggers : [];
  if (!triggers.some((trigger) => trigger.type === "pr.synchronize")) {
    return "missing pr.synchronize trigger";
  }
  return null;
}

function smokeRunSummary() {
  return {
    id: "workflow_run_smoke",
    workflowDefinitionId: "workflow_def_smoke",
    deploymentId: "workflow_dep_smoke",
    triggerType: "pr.synchronize",
    triggerIdempotencyKey: "workflow-smoke-trigger",
    status: "succeeded",
    backend: "bullmq",
    backendInstanceId: "workflow-run:workflow_run_smoke",
    attempt: 1,
    startedAt: "2026-06-05T00:00:01.000Z",
    completedAt: "2026-06-05T00:00:02.000Z",
    createdAt: "2026-06-05T00:00:00.000Z",
    updatedAt: "2026-06-05T00:00:02.000Z",
    definition: {
      id: "workflow_def_smoke",
      slug: "pr-risk-review",
      name: "PR Risk Review",
    },
    deployment: {
      id: "workflow_dep_smoke",
      version: "0.1.0",
      backend: "bullmq",
      status: "active",
    },
  };
}

function smokeArtifact() {
  return {
    id: "workflow_artifact_smoke",
    runId: "workflow_run_smoke",
    stepId: "workflow_step_smoke",
    storageBackend: "local-file",
    objectKey: "workflow-artifacts/smoke.json",
    contentType: "application/json",
    sizeBytes: 52,
    digest: "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    createdAt: "2026-06-05T00:00:02.000Z",
  };
}

function smokeTokenLease() {
  return {
    id: "workflow_token_lease_smoke",
    runId: "workflow_run_smoke",
    stepId: "workflow_step_smoke",
    provider: "github",
    scmConnectionId: "scm_conn_smoke",
    repoId: "repo_smoke",
    requestedPermissions: ["pull_requests.read"],
    expiresAt: "2026-06-05T00:10:02.000Z",
    createdAt: "2026-06-05T00:00:02.000Z",
  };
}

function smokeLog() {
  return {
    id: "workflow_log_smoke",
    runId: "workflow_run_smoke",
    stepId: null,
    level: "info",
    message: "workflow started",
    fields: { source: "smoke" },
    createdAt: "2026-06-05T00:00:01.000Z",
  };
}

function smokeWorkflowDefinition(status = "active") {
  return {
    id: "workflow_def_smoke",
    slug: "pr-risk-review",
    name: "PR Risk Review",
    description: null,
    status,
    currentDeploymentId: "workflow_dep_smoke",
    currentDeployment: {
      id: "workflow_dep_smoke",
      version: "0.1.0",
      runtime: "node22",
      backend: "bullmq",
      status: "active",
      bundleDigest: "sha256:bundle-smoke",
      sourceDigest: "sha256:source-smoke",
      activatedAt: "2026-06-05T00:00:00.000Z",
      disabledAt: null,
      createdAt: "2026-06-05T00:00:00.000Z",
    },
    triggers: [
      {
        id: "workflow_trigger_smoke",
        triggerType: "pr.synchronize",
        enabled: true,
        filter: {},
        createdAt: "2026-06-05T00:00:00.000Z",
      },
    ],
    createdAt: "2026-06-05T00:00:00.000Z",
    updatedAt: "2026-06-05T00:00:00.000Z",
  };
}

function deploymentControl(status, extra = {}) {
  return {
    workflowDefinitionId: "workflow_def_smoke",
    workflowDeploymentId: "workflow_dep_smoke",
    status,
    ...extra,
  };
}

function runtimeKeyResponse(revoked, includeKey = true) {
  return {
    workflowDefinitionId: "workflow_def_smoke",
    workflowDeploymentId: "workflow_dep_smoke",
    key: includeKey
      ? {
          id: "workflow_runtime_key_smoke",
          keyHash: "sha256:runtime-key-smoke",
          permissions: { workflow: ["run"] },
          expiresAt: null,
          revokedAt: null,
          createdAt: "2026-06-05T00:00:00.000Z",
        }
      : null,
    revoked,
  };
}

function secretSummary() {
  return {
    name: "SLACK_WEBHOOK_URL",
    createdAt: "2026-06-05T00:00:00.000Z",
    updatedAt: "2026-06-05T00:00:00.000Z",
  };
}

function notificationRouteSummary(enabled = true) {
  return {
    id: "workflow_notification_route_smoke",
    channel: "alerts",
    transport: "webhook",
    targetHost: "hooks.example",
    enabled,
    createdAt: "2026-06-05T00:00:00.000Z",
    updatedAt: "2026-06-05T00:00:00.000Z",
  };
}

const server = http.createServer(async (req, res) => {
  try {
    const url = new URL(req.url, "http://127.0.0.1");
    if (req.method === "GET" && url.pathname === "/api/workflows/capabilities") {
      sendJson(res, 200, {
        sdk: {
          sdkPackage: "@git-ai-project/workflows",
          supportedVersions: ["0.0.0"],
          versionPolicy: "exact",
        },
      });
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/workflows/upload") {
      const body = await readJson(req);
      const validationError = validateUpload(req, body);
      if (validationError) {
        rejectUpload(res, validationError);
        return;
      }

      fs.appendFileSync(
        logFile,
        JSON.stringify({
          method: req.method,
          path: url.pathname,
          apiKey: req.headers["x-api-key"],
          request: body,
        }) + "\n",
      );
      sendJson(res, 201, {
        organizationId: "org_smoke",
        workflowDefinitionId: "workflow_def_smoke",
        workflowDeploymentId: "workflow_dep_smoke",
        workflowBundleId: "workflow_bundle_smoke",
        workflowTriggerIds: ["workflow_trigger_smoke"],
        workflowDefinitionStatus: "active",
        workflowDeploymentStatus: "active",
        activated: Boolean(body.activate),
        reviewRequired: false,
        reviewReasons: [],
      });
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/workflows") {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, { workflows: [smokeWorkflowDefinition()] });
      return;
    }

    const deploymentControlMatch = url.pathname.match(
      /^\/api\/workflows\/definitions\/workflow_def_smoke\/deployments\/workflow_dep_smoke\/(approve|reject|activate|disable|rollback)$/,
    );
    if (req.method === "POST" && deploymentControlMatch) {
      if (!requireApiKey(req, res)) {
        return;
      }
      const action = deploymentControlMatch[1];
      const statusByAction = {
        approve: "approved",
        reject: "rejected",
        activate: "active",
        disable: "disabled",
        rollback: "active",
      };
      const extra =
        action === "rollback"
          ? { rolledBackFromDeploymentId: "workflow_dep_previous" }
          : {};
      sendJson(res, 200, deploymentControl(statusByAction[action], extra));
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname === "/api/workflows/definitions/workflow_def_smoke/archive"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, {
        workflowDefinitionId: "workflow_def_smoke",
        status: "archived",
        archivedDeployments: 1,
      });
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname === "/api/workflows/definitions/workflow_def_smoke/restore"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, {
        workflowDefinitionId: "workflow_def_smoke",
        status: "disabled",
      });
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname ===
        "/api/workflows/definitions/workflow_def_smoke/deployments/workflow_dep_smoke/runtime-key/rotate"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, runtimeKeyResponse(1, true));
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname ===
        "/api/workflows/definitions/workflow_def_smoke/deployments/workflow_dep_smoke/runtime-key/revoke"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, runtimeKeyResponse(1, false));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/workflows/triggers/pr.synchronize") {
      if (!requireApiKey(req, res)) {
        return;
      }
      const body = await readJson(req);
      fs.appendFileSync(
        logFile,
        JSON.stringify({
          method: req.method,
          path: url.pathname,
          apiKey: req.headers["x-api-key"],
          request: body,
        }) + "\n",
      );
      if (body?.event?.type !== "pr.synchronize" || body.unique !== true) {
        sendJson(res, 400, { error: "unexpected pr.synchronize trigger request" });
        return;
      }
      sendJson(res, 202, {
        accepted: true,
        eventId: "workflow_event_smoke",
        eventType: "pr.synchronize",
        organizationId: "org_smoke",
        idempotencyKey: "workflow-smoke-trigger",
        unique: true,
      });
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname === "/api/workflows/triggers/pr.synchronize/backfill"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      const body = await readJson(req);
      fs.appendFileSync(
        logFile,
        JSON.stringify({
          method: req.method,
          path: url.pathname,
          apiKey: req.headers["x-api-key"],
          request: body,
        }) + "\n",
      );
      if (
        body?.from !== "2026-06-01T00:00:00.000Z" ||
        body?.to !== "2026-06-05T00:00:00.000Z" ||
        body?.dryRun !== true ||
        body?.limit !== 2 ||
        body?.idempotencyKeySuffix !== "smoke" ||
        !Array.isArray(body?.repositories) ||
        body.repositories[0] !== "acme/widgets" ||
        !Array.isArray(body?.providers) ||
        body.providers[0] !== "azure-devops" ||
        !Array.isArray(body?.prNumbers) ||
        body.prNumbers[0] !== 42
      ) {
        sendJson(res, 400, { error: "unexpected pr.synchronize backfill request" });
        return;
      }
      sendJson(res, 202, {
        accepted: true,
        dryRun: true,
        scanned: 2,
        matched: 1,
        enqueued: 0,
        skipped: 1,
        events: [
          {
            eventId: "workflow_event_backfill_smoke",
            idempotencyKey: "pr.synchronize:org_smoke:repo_smoke:42:7:smoke",
            repository: "acme/widgets",
            pullNumber: 42,
            latestSyncSeq: 7,
            occurredAt: "2026-06-05T00:00:00.000Z",
            dryRun: true,
            enqueued: false,
          },
        ],
      });
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/workflows/runs") {
      if (!requireApiKey(req, res)) {
        return;
      }
      fs.appendFileSync(
        logFile,
        JSON.stringify({
          method: req.method,
          path: url.pathname,
          apiKey: req.headers["x-api-key"],
          query: Object.fromEntries(url.searchParams.entries()),
        }) + "\n",
      );
      sendJson(res, 200, { runs: [smokeRunSummary()] });
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/workflows/runs/workflow_run_smoke/logs") {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, {
        run: smokeRunSummary(),
        logs: [smokeLog()],
      });
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/workflows/runs/workflow_run_smoke") {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, {
        ...smokeRunSummary(),
        eventPayload: { type: "pr.synchronize", pullRequest: { number: 42 } },
        output: { risk: "medium" },
        error: null,
        steps: [
          {
            id: "workflow_step_smoke",
            stepKey: "summarize",
            stepName: "summarize",
            stepType: "do",
            status: "succeeded",
            attempt: 1,
            outputArtifactId: "workflow_artifact_smoke",
            error: null,
            startedAt: "2026-06-05T00:00:01.000Z",
            completedAt: "2026-06-05T00:00:02.000Z",
            createdAt: "2026-06-05T00:00:01.000Z",
          },
        ],
        waits: [],
        artifacts: [smokeArtifact()],
        tokenLeases: [smokeTokenLease()],
        recentLogs: [smokeLog()],
      });
      return;
    }

    if (
      req.method === "GET" &&
      url.pathname === "/api/workflows/runs/workflow_run_smoke/artifacts/workflow_artifact_smoke"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, {
        risk: "medium",
        reviewed: true,
        source: "workflow-smoke",
      });
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname === "/api/workflows/runs/workflow_run_smoke/cancel"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 202, {
        accepted: true,
        action: "cancel",
        runId: "workflow_run_smoke",
        backend: "bullmq",
      });
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname === "/api/workflows/runs/workflow_run_smoke/restart"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      const body = await readJson(req);
      if (body?.fromStep !== "summarize") {
        sendJson(res, 400, { error: "restart did not include expected fromStep" });
        return;
      }
      sendJson(res, 202, {
        accepted: true,
        action: "restart",
        runId: "workflow_run_smoke",
        backend: "bullmq",
      });
      return;
    }

    if (
      req.method === "POST" &&
      url.pathname === "/api/workflows/runs/workflow_run_smoke/refresh"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, {
        runId: "workflow_run_smoke",
        backend: "bullmq",
        backendInstanceId: "workflow-run:workflow_run_smoke",
        status: "succeeded",
      });
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/workflows/secrets") {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, { secrets: [secretSummary()] });
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/workflows/secrets") {
      if (!requireApiKey(req, res)) {
        return;
      }
      const body = await readJson(req);
      if (body?.name !== "SLACK_WEBHOOK_URL" || body?.value !== "https://hooks.example/smoke") {
        sendJson(res, 400, { error: "unexpected secret set request" });
        return;
      }
      sendJson(res, 201, { secret: secretSummary(), created: true });
      return;
    }

    if (
      req.method === "DELETE" &&
      url.pathname === "/api/workflows/secrets/SLACK_WEBHOOK_URL"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, { deleted: true });
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/workflows/notification-routes") {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, { routes: [notificationRouteSummary()] });
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/workflows/notification-routes") {
      if (!requireApiKey(req, res)) {
        return;
      }
      const body = await readJson(req);
      if (
        body?.channel !== "alerts" ||
        body?.transport !== "webhook" ||
        body?.targetUrl !== "https://hooks.example/smoke" ||
        body?.enabled !== true
      ) {
        sendJson(res, 400, { error: "unexpected notification route set request" });
        return;
      }
      sendJson(res, 201, { route: notificationRouteSummary(true), created: true });
      return;
    }

    if (
      req.method === "DELETE" &&
      url.pathname === "/api/workflows/notification-routes/alerts"
    ) {
      if (!requireApiKey(req, res)) {
        return;
      }
      sendJson(res, 200, { deleted: true });
      return;
    }

    sendJson(res, 404, { error: `unexpected ${req.method} ${url.pathname}` });
  } catch (error) {
    sendJson(res, 500, { error: error.message });
  }
});

server.listen(0, "127.0.0.1", () => {
  fs.writeFileSync(portFile, String(server.address().port));
});

process.on("SIGTERM", () => {
  server.close(() => process.exit(0));
});
NODE
  mock_api_pid=$!

  for _ in $(seq 1 50); do
    if [ -s "$mock_api_port_file" ]; then
      return
    fi
    sleep 0.1
  done

  echo "Error: local workflow API mock did not start" >&2
  exit 1
}

mkdir -p "$sdk_copy"
if command -v rsync >/dev/null 2>&1; then
  rsync -a \
    --exclude node_modules \
    --exclude dist \
    --exclude .wrangler \
    "$sdk_root"/ "$sdk_copy"/
else
  cp -R "$sdk_root"/. "$sdk_copy"/
  rm -rf "$sdk_copy/node_modules" "$sdk_copy/dist" "$sdk_copy/.wrangler"
fi

echo "Building local workflow SDK copy from $sdk_root"
(cd "$sdk_copy" && npm exec --yes --package "typescript@^5.0.0" -- tsc -p tsconfig.json)

echo "Building git-ai debug binary"
(cd "$cli_root" && cargo build --quiet --bin git-ai)
git_ai="$cli_root/target/debug/git-ai"

echo "Initializing generated workflow project"
"$git_ai" workflows init "PR Risk Review" --dir "$project_dir"

echo "Installing generated workflow dependencies in temp project"
npm install --silent --prefix "$project_dir" \
  "$sdk_copy" \
  esbuild@0.25.0 \
  tsx@4.20.6 \
  "typescript@^5.0.0"

echo "Validating generated workflow manifest"
"$git_ai" workflows validate --manifest "$project_dir/gitai.workflow.json"

echo "Bundling generated workflow"
"$git_ai" workflows bundle \
  --manifest "$project_dir/gitai.workflow.json" \
  --out "$bundle_dir"

test -s "$bundle_dir/bundle.js"
test -s "$bundle_dir/manifest.json"
test -s "$bundle_dir/source-digest.txt"
test -s "$bundle_dir/bundle-digest.txt"

echo "Running generated workflow fixture through local dev runner"
dev_output="$("$git_ai" workflows dev \
  --manifest "$project_dir/gitai.workflow.json" \
  --event "$project_dir/fixtures/pr.synchronize.json" \
  --json)"

DEV_OUTPUT="$dev_output" node <<'NODE'
const raw = process.env.DEV_OUTPUT;
let output;
try {
  output = JSON.parse(raw);
} catch (error) {
  console.error("workflow dev did not emit JSON");
  console.error(raw);
  throw error;
}

const hasSummarizeStep = output.steps?.some(
  (step) => step.type === "do" && step.name === "summarize",
);
if (!hasSummarizeStep) {
  throw new Error("workflow dev output did not include the summarize step");
}

const hasStartedLog = output.logs?.some(
  (log) => log.level === "info" && log.message === "workflow started",
);
if (!hasStartedLog) {
  throw new Error("workflow dev output did not include the scaffold log line");
}

const hasScmLeaseLog = output.logs?.some(
  (log) =>
    log.level === "info" &&
    log.message === "workflow scm token leased" &&
    log.fields?.provider === "github" &&
    log.fields?.leaseId === "test-github-lease" &&
    log.fields?.authorizationHeader === "[REDACTED]" &&
    log.fields?.accessToken === "[REDACTED]",
);
if (!hasScmLeaseLog) {
  throw new Error("workflow dev output did not include a redacted SCM token lease log");
}

if (output.result?.scm?.provider !== "github") {
  throw new Error("workflow dev output did not include the SCM provider result");
}
if (output.result.scm.leaseId !== "test-github-lease") {
  throw new Error("workflow dev output did not include the safe SCM lease id");
}
if (output.result.scm.authType !== "bearer") {
  throw new Error("workflow dev output did not include the SCM authorization type");
}
if (output.result.scm.authorizationHeader !== "[REDACTED]") {
  throw new Error("workflow dev output did not redact the SCM authorization header");
}
if (output.result.scm.accessToken !== "[REDACTED]") {
  throw new Error("workflow dev output did not redact the SCM access token");
}
if (raw.includes("test-github-token")) {
  throw new Error("workflow dev output leaked the test SCM access token");
}
NODE

echo "Uploading generated workflow bundle through local API mock"
start_mock_api
mock_api_port="$(cat "$mock_api_port_file")"
mkdir -p "$mock_home/.git-ai"
printf '%s\n' \
  "{" \
  "  \"api_base_url\": \"http://127.0.0.1:${mock_api_port}\"," \
  "  \"api_key\": \"workflow-smoke-api-key\"" \
  "}" \
  > "$mock_home/.git-ai/config.json"

run_git_ai_api() {
  env -u GIT_AI_API_BASE_URL -u GIT_AI_API_KEY \
    HOME="$mock_home" \
    USER="workflow-smoke" \
    HOSTNAME="workflow-smoke.local" \
    "$git_ai" "$@"
}

upload_output="$(run_git_ai_api workflows upload \
    --manifest "$project_dir/gitai.workflow.json" \
    --bundle "$bundle_dir/bundle.js" \
    --backend bullmq \
    --activate)"

case "$upload_output" in
  *"Workflow definition: workflow_def_smoke"* ) ;;
  * )
    echo "workflow upload output did not include the fake definition id" >&2
    echo "$upload_output" >&2
    exit 1
    ;;
esac

case "$upload_output" in
  *"Workflow deployment: workflow_dep_smoke"* ) ;;
  * )
    echo "workflow upload output did not include the fake deployment id" >&2
    echo "$upload_output" >&2
    exit 1
    ;;
esac

test -s "$mock_api_log"
node - "$mock_api_log" <<'NODE'
const fs = require("node:fs");
const [, , logFile] = process.argv;
const entries = fs.readFileSync(logFile, "utf8")
  .trim()
  .split(/\n+/)
  .filter(Boolean)
  .map((line) => JSON.parse(line));

const upload = entries.find((entry) => entry.path === "/api/workflows/upload");
if (!upload) {
  throw new Error("mock API did not receive an upload request");
}
if (upload.apiKey !== "workflow-smoke-api-key") {
  throw new Error("upload request did not include the configured API key");
}
if (upload.request.activate !== true) {
  throw new Error("upload request did not request activation");
}
if (upload.request.deployment.backend !== "bullmq") {
  throw new Error("upload request did not target bullmq");
}
if (!upload.request.bundle.contentBase64) {
  throw new Error("upload request omitted bundle content");
}
NODE

echo "Triggering and inspecting workflow run through local API mock"
list_output="$(run_git_ai_api workflows list --json)"
approve_output="$(run_git_ai_api workflows approve workflow_def_smoke workflow_dep_smoke)"
reject_output="$(run_git_ai_api workflows reject workflow_def_smoke workflow_dep_smoke)"
activate_output="$(run_git_ai_api workflows activate workflow_def_smoke workflow_dep_smoke)"
disable_output="$(run_git_ai_api workflows disable workflow_def_smoke workflow_dep_smoke)"
rollback_output="$(run_git_ai_api workflows rollback workflow_def_smoke workflow_dep_smoke)"
archive_output="$(run_git_ai_api workflows archive workflow_def_smoke)"
restore_output="$(run_git_ai_api workflows restore workflow_def_smoke)"
runtime_key_rotate_output="$(run_git_ai_api workflows runtime-key rotate workflow_def_smoke workflow_dep_smoke)"
runtime_key_revoke_output="$(run_git_ai_api workflows runtime-key revoke workflow_def_smoke workflow_dep_smoke)"
secret_set_output="$(run_git_ai_api workflows secrets set SLACK_WEBHOOK_URL --value https://hooks.example/smoke)"
secrets_output="$(run_git_ai_api workflows secrets list --json)"
secret_delete_output="$(run_git_ai_api workflows secrets delete SLACK_WEBHOOK_URL)"
notification_route_set_output="$(run_git_ai_api workflows notifications routes set alerts --transport webhook --target https://hooks.example/smoke)"
notification_routes_output="$(run_git_ai_api workflows notifications routes list --json)"
notification_route_delete_output="$(run_git_ai_api workflows notifications routes delete alerts)"
trigger_output="$(run_git_ai_api workflows trigger pr.synchronize \
  --fixture "$project_dir/fixtures/pr.synchronize.json" \
  --idempotency-key-suffix smoke \
  --json)"
backfill_output="$(run_git_ai_api workflows backfill pr.synchronize \
  --from 2026-06-01T00:00:00.000Z \
  --to 2026-06-05T00:00:00.000Z \
  --repo acme/widgets \
  --provider ado \
  --pr 42 \
  --limit 2 \
  --dry-run \
  --idempotency-key-suffix smoke \
  --json)"
runs_output="$(run_git_ai_api workflows runs workflow_def_smoke \
  --status succeeded \
  --limit 5 \
  --json)"
inspect_output="$(run_git_ai_api workflows inspect workflow_run_smoke --json)"
logs_output="$(run_git_ai_api workflows logs workflow_run_smoke \
  --level info \
  --limit 10 \
  --json)"
artifacts_output="$(run_git_ai_api workflows artifacts workflow_run_smoke --json)"
artifact_out="$tmpdir/workflow-artifact.json"
run_git_ai_api workflows artifacts workflow_run_smoke workflow_artifact_smoke --out "$artifact_out" >/dev/null
refresh_output="$(run_git_ai_api workflows refresh workflow_run_smoke)"
restart_output="$(run_git_ai_api workflows restart workflow_run_smoke --from-step summarize)"
cancel_output="$(run_git_ai_api workflows cancel workflow_run_smoke)"

LIST_OUTPUT="$list_output" \
APPROVE_OUTPUT="$approve_output" \
REJECT_OUTPUT="$reject_output" \
ACTIVATE_OUTPUT="$activate_output" \
DISABLE_OUTPUT="$disable_output" \
ROLLBACK_OUTPUT="$rollback_output" \
ARCHIVE_OUTPUT="$archive_output" \
RESTORE_OUTPUT="$restore_output" \
RUNTIME_KEY_ROTATE_OUTPUT="$runtime_key_rotate_output" \
RUNTIME_KEY_REVOKE_OUTPUT="$runtime_key_revoke_output" \
SECRET_SET_OUTPUT="$secret_set_output" \
SECRETS_OUTPUT="$secrets_output" \
SECRET_DELETE_OUTPUT="$secret_delete_output" \
NOTIFICATION_ROUTE_SET_OUTPUT="$notification_route_set_output" \
NOTIFICATION_ROUTES_OUTPUT="$notification_routes_output" \
NOTIFICATION_ROUTE_DELETE_OUTPUT="$notification_route_delete_output" \
TRIGGER_OUTPUT="$trigger_output" \
BACKFILL_OUTPUT="$backfill_output" \
RUNS_OUTPUT="$runs_output" \
INSPECT_OUTPUT="$inspect_output" \
LOGS_OUTPUT="$logs_output" \
ARTIFACTS_OUTPUT="$artifacts_output" \
ARTIFACT_OUT="$artifact_out" \
REFRESH_OUTPUT="$refresh_output" \
RESTART_OUTPUT="$restart_output" \
CANCEL_OUTPUT="$cancel_output" \
node <<'NODE'
const fs = require("node:fs");

const list = JSON.parse(process.env.LIST_OUTPUT);
if (!list.workflows?.some((workflow) => workflow.id === "workflow_def_smoke")) {
  throw new Error("list command did not return the smoke workflow");
}

const expectedOutputFragments = [
  ["approve", process.env.APPROVE_OUTPUT, "Approved workflow deployment workflow_dep_smoke"],
  ["reject", process.env.REJECT_OUTPUT, "Rejected workflow deployment workflow_dep_smoke"],
  ["activate", process.env.ACTIVATE_OUTPUT, "Activated workflow deployment workflow_dep_smoke"],
  ["disable", process.env.DISABLE_OUTPUT, "Disabled workflow deployment workflow_dep_smoke"],
  ["rollback", process.env.ROLLBACK_OUTPUT, "Rolled back workflow definition workflow_def_smoke"],
  ["archive", process.env.ARCHIVE_OUTPUT, "Archived workflow definition workflow_def_smoke"],
  ["restore", process.env.RESTORE_OUTPUT, "Restored workflow definition workflow_def_smoke"],
  ["runtime-key rotate", process.env.RUNTIME_KEY_ROTATE_OUTPUT, "Rotated runtime key for workflow deployment workflow_dep_smoke"],
  ["runtime-key revoke", process.env.RUNTIME_KEY_REVOKE_OUTPUT, "Revoked 1 runtime key(s) for workflow deployment workflow_dep_smoke"],
  ["secret set", process.env.SECRET_SET_OUTPUT, "Created workflow secret SLACK_WEBHOOK_URL"],
  ["secret delete", process.env.SECRET_DELETE_OUTPUT, "Deleted workflow secret SLACK_WEBHOOK_URL"],
  ["notification route set", process.env.NOTIFICATION_ROUTE_SET_OUTPUT, "Created workflow notification route alerts"],
  ["notification route delete", process.env.NOTIFICATION_ROUTE_DELETE_OUTPUT, "Deleted workflow notification route alerts"],
];
for (const [name, output, fragment] of expectedOutputFragments) {
  if (!output.includes(fragment)) {
    throw new Error(`${name} command output was unexpected: ${output}`);
  }
}

const secrets = JSON.parse(process.env.SECRETS_OUTPUT);
if (!secrets.secrets?.some((secret) => secret.name === "SLACK_WEBHOOK_URL")) {
  throw new Error("secrets list command did not return the smoke secret");
}

const routes = JSON.parse(process.env.NOTIFICATION_ROUTES_OUTPUT);
if (
  !routes.routes?.some(
    (route) =>
      route.channel === "alerts" &&
      route.transport === "webhook" &&
      route.targetHost === "hooks.example",
  )
) {
  throw new Error("notification routes list command did not return the smoke route");
}

const trigger = JSON.parse(process.env.TRIGGER_OUTPUT);
if (trigger.eventId !== "workflow_event_smoke" || trigger.unique !== true) {
  throw new Error("trigger command did not return expected event response");
}

const backfill = JSON.parse(process.env.BACKFILL_OUTPUT);
if (
  backfill.accepted !== true ||
  backfill.dryRun !== true ||
  backfill.scanned !== 2 ||
  backfill.matched !== 1 ||
  backfill.skipped !== 1 ||
  !backfill.events?.some(
    (event) =>
      event.eventId === "workflow_event_backfill_smoke" &&
      event.repository === "acme/widgets" &&
      event.pullNumber === 42 &&
      event.latestSyncSeq === 7 &&
      event.enqueued === false,
  )
) {
  throw new Error("backfill command did not return expected dry-run response");
}

const runs = JSON.parse(process.env.RUNS_OUTPUT);
if (!runs.runs?.some((run) => run.id === "workflow_run_smoke" && run.status === "succeeded")) {
  throw new Error("runs command did not return the smoke run");
}

const inspected = JSON.parse(process.env.INSPECT_OUTPUT);
if (
  inspected.id !== "workflow_run_smoke" ||
  !inspected.tokenLeases?.some(
    (lease) =>
      lease.id === "workflow_token_lease_smoke" &&
      lease.provider === "github" &&
      lease.stepId === "workflow_step_smoke" &&
      lease.requestedPermissions?.[0] === "pull_requests.read" &&
      lease.accessToken === undefined &&
      lease.authorization === undefined,
  )
) {
  throw new Error("inspect command did not return safe token lease audit metadata");
}

const logs = JSON.parse(process.env.LOGS_OUTPUT);
if (!logs.logs?.some((log) => log.message === "workflow started")) {
  throw new Error("logs command did not return the smoke log");
}

const artifacts = JSON.parse(process.env.ARTIFACTS_OUTPUT);
if (!artifacts.artifacts?.some((artifact) => artifact.id === "workflow_artifact_smoke")) {
  throw new Error("artifacts list did not include the smoke artifact");
}

const artifact = JSON.parse(fs.readFileSync(process.env.ARTIFACT_OUT, "utf8"));
if (artifact.risk !== "medium" || artifact.reviewed !== true) {
  throw new Error("artifact fetch did not write expected JSON content");
}

if (!process.env.REFRESH_OUTPUT.includes("Refreshed workflow run workflow_run_smoke")) {
  throw new Error("refresh command output was unexpected");
}
if (!process.env.RESTART_OUTPUT.includes("Accepted restart for workflow run workflow_run_smoke")) {
  throw new Error("restart command output was unexpected");
}
if (!process.env.CANCEL_OUTPUT.includes("Accepted cancel for workflow run workflow_run_smoke")) {
  throw new Error("cancel command output was unexpected");
}
NODE

echo "Workflow CLI local smoke passed"
