# Custom Workflows CLI Contract

## Purpose

Add a `git-ai workflows` command group that lets organization developers build, test, upload, activate, and inspect custom Git AI workflows authored with `@git-ai-project/workflows`.

This CLI contract pairs with the monorepo design spec:

```text
docs/superpowers/specs/2026-06-05-custom-workflows-dual-backend-design.md
```

The CLI must not expose backend-specific concepts such as Cloudflare Dynamic Workers or BullMQ jobs. It works against the Git AI workflow API and the portable SDK package.

## Commands

```text
git-ai workflows init [name]
git-ai workflows dev --event <fixture> [--watch]
git-ai workflows validate
git-ai workflows bundle
git-ai workflows upload [--org <org>] [--signature-file <path> --signature-key-id <id>] [--activate]
git-ai workflows list [--org <org>]
git-ai workflows activate <workflow-definition-id> <workflow-deployment-id>
git-ai workflows approve <workflow-definition-id> <workflow-deployment-id>
git-ai workflows reject <workflow-definition-id> <workflow-deployment-id>
git-ai workflows disable <workflow-definition-id> <workflow-deployment-id>
git-ai workflows rollback <workflow-definition-id> <workflow-deployment-id>
git-ai workflows archive <workflow-definition-id>
git-ai workflows restore <workflow-definition-id>
git-ai workflows runtime-key rotate <workflow-definition-id> <workflow-deployment-id>
git-ai workflows runtime-key revoke <workflow-definition-id> <workflow-deployment-id>
git-ai workflows runs [workflow]
git-ai workflows inspect <run-id> [--json]
git-ai workflows logs <run-id> [--follow]
git-ai workflows artifacts <run-id> [artifact-id] [--out <path>] [--json]
git-ai workflows cancel <run-id>
git-ai workflows refresh <run-id>
git-ai workflows restart <run-id> [--from-step <step-name-or-key>]
git-ai workflows secrets list [--json]
git-ai workflows secrets set <name> (--value <value>|--value-stdin)
git-ai workflows secrets delete <name>
git-ai workflows notifications routes list [--json]
git-ai workflows notifications routes set <channel> --transport webhook|email|scm-pr-comment [--target <url-or-email>] [--disabled]
git-ai workflows notifications routes delete <channel>
git-ai workflows trigger pr.synchronize --fixture <file>
git-ai workflows backfill pr.synchronize [--from <iso>] [--to <iso>] [--repo <id|full-name|url>] [--provider github|gitlab|bitbucket|azure-devops|ado] [--pr <number>] [--dry-run]
```

Implemented in this spike:

```text
git-ai workflows init [name] [--dir <dir>]
git-ai workflows dev [--manifest <path>] [--event <fixture>] [--watch] [--json]
git-ai workflows validate [--manifest <path>]
git-ai workflows bundle [--manifest <path>] [--out <dir>]
git-ai workflows upload [--manifest <path>] [--bundle <path>] [--backend bullmq|cloudflare] [--signature-file <path> --signature-key-id <id>] [--activate]
git-ai workflows list [--status <status>] [--limit <n>] [--json]
git-ai workflows activate <workflow-definition-id> <workflow-deployment-id>
git-ai workflows approve <workflow-definition-id> <workflow-deployment-id>
git-ai workflows reject <workflow-definition-id> <workflow-deployment-id>
git-ai workflows disable <workflow-definition-id> <workflow-deployment-id>
git-ai workflows rollback <workflow-definition-id> <workflow-deployment-id>
git-ai workflows archive <workflow-definition-id>
git-ai workflows restore <workflow-definition-id>
git-ai workflows runtime-key rotate <workflow-definition-id> <workflow-deployment-id>
git-ai workflows runtime-key revoke <workflow-definition-id> <workflow-deployment-id>
git-ai workflows runs [workflow-definition-id] [--status <status>] [--limit <n>] [--json]
git-ai workflows logs <run-id> [--level <level>] [--limit <n>] [--follow] [--json]
git-ai workflows artifacts <run-id> [artifact-id] [--out <path>] [--json]
git-ai workflows cancel <run-id>
git-ai workflows refresh <run-id>
git-ai workflows restart <run-id> [--from-step <step-name-or-key>]
git-ai workflows secrets list [--json]
git-ai workflows secrets set <name> (--value <value>|--value-stdin)
git-ai workflows secrets delete <name>
git-ai workflows notifications routes list [--json]
git-ai workflows notifications routes set <channel> --transport webhook|email|scm-pr-comment [--target <url-or-email>] [--disabled]
git-ai workflows notifications routes delete <channel>
git-ai workflows trigger pr.synchronize --fixture <file> [--reuse-idempotency-key] [--json]
git-ai workflows backfill pr.synchronize [--from <iso>] [--to <iso>] [--repo <id|full-name|url>] [--provider github|gitlab|bitbucket|azure-devops|ado] [--pr <number>] [--limit <n>] [--dry-run] [--idempotency-key-suffix <suffix>] [--json]
```

The current `bundle` command shells out to local `node_modules/.bin/esbuild` or
`npx esbuild@0.25.0`, emits an ESM bundle, externalizes
`@git-ai-project/workflows`, and writes deterministic source/bundle digests.
The current `dev` command shells out to local `node_modules/.bin/tsx` or
`npx tsx@4.20.6`, loads the workflow entrypoint and fixture, and runs the SDK
test runtime. Printed local output is recursively redacted for sensitive keys
and common bearer/GitHub/GitLab/Slack token patterns. `--watch` polls the
workflow project for source/manifest/fixture changes and reruns the same local
fixture, skipping `.git`, `.gitai`, `node_modules`, build output, and target
directories.

## Files

`git-ai workflows init` creates:

```text
gitai.workflow.ts
gitai.workflow.json
fixtures/pr.synchronize.json
package.json
tsconfig.json
```

`gitai.workflow.json` contains:

```json
{
  "schemaVersion": "workflow-manifest/1.0",
  "entrypoint": "gitai.workflow.ts",
  "runtime": "node18",
  "sdkPackage": "@git-ai-project/workflows",
  "sdkVersion": "0.0.0",
  "permissions": {
    "scm": ["contents.read", "pull_requests.read"],
    "gitAi": ["pr.read"],
    "network": []
  }
}
```

Runtime `ctx.scm.<provider>()` calls with omitted `permissions` request the
manifest's approved SCM permission list. Deployments with no approved SCM
permissions cannot obtain a provider token by sending an empty permission list;
explicit permission requests must be a subset of the approved manifest/admin
policy. The generated scaffold leases a GitHub provider client and calls
`getToken()` so local development proves the short-lived token contract used by
customer-provided SCM SDKs. `git-ai workflows dev` redacts returned access
tokens and authorization headers from local logs and output while preserving safe
metadata such as provider, lease id, and authorization type.

## Local Development

`git-ai workflows dev`:

1. Loads the workflow entrypoint.
2. Loads the event fixture.
3. Runs with the SDK in-memory test runtime.
4. Prints step execution order, logs, notifications, and final output.
5. Redacts configured secrets and token-looking strings.
6. With `--watch`, reruns when source files, manifest, or fixture change.

It does not require Docker, Cloudflare, Redis, or Postgres.
It does require Node.js/npm and a resolvable `@git-ai-project/workflows`
dependency in the workflow project.

## Validation

`git-ai workflows validate` checks:

- manifest schema;
- slug, name, version, runtime, backend, and entrypoint path;
- trigger types are known;
- `pr.synchronize` filters are valid, including repository/branch arrays,
  `open|closed|merged` states, and boolean flags;
- permission groups and permission values are known;
- workflow secret names follow the server-side naming rules;
- network allowlist entries are non-empty `http://` or `https://` patterns;
- limits use known numeric keys and positive values; `redactionLiterals`,
  `redactionPatterns`, and `redactionDetectorPacks` use validated string arrays.
- the workflow entrypoint exports exactly one default workflow definition.
- SDK metadata is present and uses `@git-ai-project/workflows`; upload checks
  `sdkVersion` against the Git AI server capabilities endpoint.
- `init` emits manifest `sdkVersion` and `package.json` dependency version from
  one CLI constant; the workflow test suite covers that scaffold contract.
- SDK upgrades must add or update the matching
  `packages/workflows/compatibility/<sdkVersion>` fixture. The package
  compatibility check runs those fixtures through
  `@git-ai-project/workflows/testing`, so CLI scaffold constants, server
  capabilities, package exports, and executable customer workflow examples move
  together.

## Bundling

`git-ai workflows bundle` produces:

```text
.gitai/workflows/<workflow-id>/<version>/
  bundle.js
  manifest.json
  source-digest.txt
  bundle-digest.txt
```

Bundle rules:

- deterministic output;
- no environment variables in bundle;
- no local absolute paths in source maps by default;
- package lock digest included when available;
- bundle digest is SHA-256 over `bundle.js` and normalized manifest.
- bundle size is capped before upload using the server's
  `WORKFLOW_BUNDLE_MAX_BYTES` default of 10 MB unless overridden locally.
- `upload --bundle <path>` verifies adjacent `manifest.json`,
  `source-digest.txt`, and `bundle-digest.txt` when present. A stale manifest or
  bundle digest fails upload locally with a rebuild message instead of sending
  inconsistent bundle metadata to the server.

## Upload

`git-ai workflows upload`:

1. Resolves auth from the normal Git AI credential store.
2. Uploads manifest, bundle digest, and base64-encoded bundle bytes.
3. Server stores the bundle in configured shared bundle storage and records the
   resulting storage backend/object key.
4. Prints the created workflow definition/deployment IDs.
5. Prints the lifecycle status returned by the server. Definition status is the
   post-refresh workflow status, so an existing active deployment can remain
   active while a newly uploaded version waits for review.
6. If review is required, prints the review reasons, dashboard review URL when
   organization metadata is present, and the exact `git-ai workflows approve`
   command.
7. If the deployment is uploaded but not active, prints the exact
   `git-ai workflows activate` command.
8. If `--activate` is set, requests activation as part of upload. The server
   still leaves the deployment in `pending_review` when the organization's
   review policy requires approval.
9. If the resolved workflow definition is archived, the server rejects upload;
   the workflow must be restored or recreated with a new slug before new
   deployments can be added.

API endpoints:

```text
POST /api/workflows/upload
GET  /api/workflows/capabilities
GET  /api/workflows
POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/approve
POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/reject
POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/activate
POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/disable
POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/rollback
POST /api/workflows/definitions/:workflowDefinitionId/archive
POST /api/workflows/definitions/:workflowDefinitionId/restore
GET  /api/workflows/runs
GET  /api/workflows/runs/:runId
GET  /api/workflows/runs/:runId/logs
GET  /api/workflows/runs/:runId/artifacts/:artifactId
POST /api/workflows/runs/:runId/cancel
POST /api/workflows/runs/:runId/refresh
POST /api/workflows/runs/:runId/restart
GET  /api/workflows/secrets
POST /api/workflows/secrets
DELETE /api/workflows/secrets/:name
GET  /api/workflows/notification-routes
POST /api/workflows/notification-routes
DELETE /api/workflows/notification-routes/:channel
POST /api/workflows/triggers/pr.synchronize
```

Initial upload request body:

```json
{
  "activate": true,
  "definition": {
    "slug": "pr-risk-review",
    "name": "PR Risk Review",
    "description": "Classifies pull request risk"
  },
  "deployment": {
    "version": "1.0.0",
    "runtime": "node22",
    "backend": "bullmq",
    "bundleDigest": "sha256:...",
    "sourceDigest": "sha256:...",
    "manifestJson": {},
    "permissionsJson": {
      "scm": ["pull_requests.write"],
      "gitAi": ["pr.read"]
    },
    "limitsJson": {
      "timeoutMs": 30000
    }
  },
  "bundle": {
    "storageBackend": "inline",
    "objectKey": "inline:<bundle-digest>",
    "sizeBytes": 12345,
    "contentBase64": "<base64 bundle.js>",
    "contentType": "text/javascript",
    "signature": {
      "keyId": "customer_key_1",
      "algorithm": "ed25519",
      "signature": "<base64 detached signature over bundle.js>"
    }
  },
  "triggers": [
    {
      "type": "pr.synchronize",
      "filter": {
        "repositories": ["acme/*"],
        "branches": ["main"],
        "states": ["open"],
        "materialChangesOnly": true
      }
    }
  ]
}
```

Upload response body:

```json
{
  "organizationId": "org_1",
  "workflowDefinitionId": "workflow_def_1",
  "workflowDeploymentId": "workflow_dep_1",
  "workflowBundleId": "workflow_bundle_1",
  "workflowTriggerIds": ["workflow_trigger_1"],
  "workflowDefinitionStatus": "pending_review",
  "workflowDeploymentStatus": "pending_review",
  "activated": false,
  "reviewRequired": true,
  "reviewReasons": ["SCM write permissions"]
}
```

The server derives `organizationId` from the API key reference, verifies bundle
size/digest metadata, stores `contentBase64` into shared bundle storage, and
persists the stored `workflow_bundle.storage_backend/object_key/digest`.
When `--signature-file` and `--signature-key-id` are supplied, the CLI sends
`bundle.signature`; the server verifies the detached Ed25519 signature against
operator-configured public keys before storing the deployment. Servers may set
`WORKFLOW_BUNDLE_SIGNATURE_REQUIRED=true|1` to reject unsigned uploads.
`activate: true` requires both `workflow.definition.write` and
`workflow.definition.activate`, but it does not bypass review policy.
Post-upload review, activation, disablement, and rollback use deployment-specific endpoints:

```http
POST /api/workflows/definitions/<definition-id>/deployments/<deployment-id>/approve
X-API-Key: <org api key with workflow.definition.review>

POST /api/workflows/definitions/<definition-id>/deployments/<deployment-id>/reject
X-API-Key: <org api key with workflow.definition.review>

POST /api/workflows/definitions/<definition-id>/deployments/<deployment-id>/activate
X-API-Key: <org api key with workflow.definition.activate>

POST /api/workflows/definitions/<definition-id>/deployments/<deployment-id>/disable
X-API-Key: <org api key with workflow.definition.disable>

POST /api/workflows/definitions/<definition-id>/deployments/<deployment-id>/rollback
X-API-Key: <org api key with workflow.definition.rollback>

POST /api/workflows/definitions/<definition-id>/archive
X-API-Key: <org api key with workflow.definition.disable>

POST /api/workflows/definitions/<definition-id>/restore
X-API-Key: <org api key with workflow.definition.write>

POST /api/workflows/definitions/<definition-id>/deployments/<deployment-id>/runtime-key/rotate
X-API-Key: <org api key with workflow.runtime_key.rotate>

POST /api/workflows/definitions/<definition-id>/deployments/<deployment-id>/runtime-key/revoke
X-API-Key: <org api key with workflow.runtime_key.revoke>
```

Current read endpoints:

```http
GET /api/workflows?status=active&limit=50
X-API-Key: <org api key with workflow.definition.read>

GET /api/workflows/runs?workflowDefinitionId=<id>&status=waiting&limit=50
X-API-Key: <org api key with workflow.run.read>

GET /api/workflows/runs/<run-id>
X-API-Key: <org api key with workflow.run.read>

GET /api/workflows/runs/<run-id>/logs?level=warn&limit=100
X-API-Key: <org api key with workflow.run.read>
```

## Logs And Runs

`git-ai workflows runs` displays:

- run ID;
- workflow ID/version;
- trigger;
- backend;
- status;
- start/end times;
- linked PR/repo/session context.

`git-ai workflows inspect <run-id> [--json]` fetches run detail through
`GET /api/workflows/runs/:runId` and displays steps, waits, artifacts, recent
logs, and SCM token lease audit summaries. Lease output includes provider,
step/run binding, SCM connection/repository IDs, requested permissions, and
expiry timestamps, but never access tokens, authorization headers, or other token
material.

`git-ai workflows logs <run-id> --follow` streams:

- workflow logs;
- step transitions;
- warnings/errors;
- notification outcomes;
- token lease audit summaries without token values.

Follow mode polls `GET /api/workflows/runs/:runId/logs` and suppresses already
printed `workflow_log.id` values so repeated pages do not duplicate terminal
output. Human terminal output sorts each fetched page oldest-to-newest for
readability; `--json` preserves the API response order for scripts.

`git-ai workflows artifacts <run-id>` lists artifact metadata from run detail.
`git-ai workflows artifacts <run-id> <artifact-id> [--out <path>]` fetches JSON
artifact content through `GET /api/workflows/runs/:runId/artifacts/:artifactId`
and writes it to stdout or the requested file.

`git-ai workflows cancel <run-id>` requires `workflow.run.cancel`, calls
`POST /api/workflows/runs/:runId/cancel`, and prints the accepted backend
control action. The server rejects cancellation for terminal runs before
enqueuing a backend control job or calling the hosted dispatcher.

`git-ai workflows refresh <run-id>` requires `workflow.run.read`, calls
`POST /api/workflows/runs/:runId/refresh`, and prints the reconciled backend
status. Terminal runs return their persisted Git AI state without a backend
call, so refresh is idempotent after completion.

`git-ai workflows restart <run-id> [--from-step <step-name-or-key>]` requires
`workflow.run.restart`, calls `POST /api/workflows/runs/:runId/restart`, and
sends `fromStep` when provided so durable replay can re-run from a selected
step.

`git-ai workflows activate <workflow-definition-id> <workflow-deployment-id>`
requires `workflow.definition.activate`, calls the deployment-specific activate
endpoint, and prints the activated deployment status. Deployments still in
`pending_review` must first be approved with `git-ai workflows approve`.

`git-ai workflows disable <workflow-definition-id> <workflow-deployment-id>`
requires `workflow.definition.disable`, calls the deployment-specific disable
endpoint, and prints the disabled deployment status.

`git-ai workflows rollback <workflow-definition-id> <workflow-deployment-id>`
requires `workflow.definition.rollback`, calls the deployment-specific rollback
endpoint, and prints the deployment replaced by the rollback.

`git-ai workflows archive <workflow-definition-id>` requires
`workflow.definition.disable`, archives the definition and all non-archived
deployments, revokes active deployment runtime keys, and prints the number of
deployments archived.

`git-ai workflows restore <workflow-definition-id>` requires
`workflow.definition.write`, restores an archived definition to `disabled` so
the slug can accept future uploads, and does not unarchive old deployments.

`git-ai workflows runtime-key rotate <workflow-definition-id> <workflow-deployment-id>`
requires `workflow.runtime_key.rotate`, calls the deployment-specific runtime
key rotate endpoint, revokes previous active deployment runtime keys, and
prints the new deployment runtime key ID plus the number of revoked keys.

`git-ai workflows runtime-key revoke <workflow-definition-id> <workflow-deployment-id>`
requires `workflow.runtime_key.revoke`, calls the deployment-specific runtime
key revoke endpoint, and prints the number of revoked deployment runtime keys.

## Secrets

`git-ai workflows secrets list [--json]`:

- calls `GET /api/workflows/secrets`;
- prints only secret names and timestamps;
- requires an API key with `workflow.secret.read`.

`git-ai workflows secrets set <name> (--value <value>|--value-stdin)`:

- sends the value over HTTPS to Git AI, where it is encrypted for storage;
- never writes secret values to disk;
- supports `--value-stdin` to avoid shell history;
- prints only secret name and whether it was created or updated;
- requires an API key with `workflow.secret.write`.

`git-ai workflows secrets delete <name>`:

- calls `DELETE /api/workflows/secrets/:name`;
- requires an API key with `workflow.secret.delete`.

## Notification Routes

`git-ai workflows notifications routes list [--json]`:

- calls `GET /api/workflows/notification-routes`;
- prints channel, transport, enabled state, target host, and updated timestamp;
- requires an API key with `workflow.notification.read`.

`git-ai workflows notifications routes set <channel> --transport webhook|email|scm-pr-comment [--target <url-or-email>] [--disabled]`:

- calls `POST /api/workflows/notification-routes`;
- stores webhook URLs and email targets encrypted server-side;
- maps `scm-pr-comment` to the event pull request and does not accept `--target`;
- defaults to enabled unless `--disabled` is passed;
- requires an API key with `workflow.notification.write`.

`git-ai workflows notifications routes delete <channel>`:

- calls `DELETE /api/workflows/notification-routes/:channel`;
- requires an API key with `workflow.notification.delete`.

## Trigger Fixtures

`git-ai workflows trigger pr.synchronize --fixture <file>` posts a test trigger
event to the server and requires an API key with `workflow.trigger.write`.
The CLI defaults to unique test submissions so repeated development runs start
new workflow runs. Pass `--reuse-idempotency-key` to preserve exact replay and
dedupe semantics.

The local fixture format matches the server `PrSynchronizeEventPayload` envelope produced by `web/lib/workflows/events.ts`.

## Historical Backfill

`git-ai workflows backfill pr.synchronize` calls
`POST /api/workflows/triggers/pr.synchronize/backfill` and requires an API key
with `workflow.trigger.write`. It reconstructs events from historical
`git_pr_records`, joins them to current repo/SCM connection metadata, and uses
the normal workflow dispatch path.

The command supports `--from`, `--to`, repeated `--repo`, repeated
`--provider`, repeated `--pr`, `--limit`, and `--dry-run` for bounded operator
use. It reuses the original PR-sync idempotency key by default for dedupe-safe
recovery. `--idempotency-key-suffix <suffix>` appends a backfill suffix only
when the operator intentionally wants fresh workflow runs.
Provider values are transmitted in the server's canonical form; the CLI accepts
`ado` as a shorthand for `azure-devops`.

## Implementation Notes

Add a new Rust module:

```text
src/commands/workflows.rs
```

Wire it from `src/commands/git_ai_handlers.rs` under the `workflows` subcommand. Keep parsing style consistent with existing direct `git-ai` subcommands rather than introducing a second top-level Clap tree unless the command dispatcher is refactored separately.

Add API client helpers under:

```text
src/api/workflows.rs
```

Use the existing `ApiContext` so workflow commands inherit login, API key, timeout, `X-Distinct-ID`, and local `api_base_url` behavior.

## Local Spike Verification

Verified on June 5, 2026 from `~/projects/git-ai`:

```bash
cargo check
cargo fmt --check
cargo test --lib workflows
GIT_AI_WORKFLOWS_SDK_ROOT=/home/ubuntu/projects/monorepo/.worktrees/feat/explore-dynamic-workflows-cf/packages/workflows \
  task workflows:smoke
```

Observed result: Rust compile/format/workflow tests passed; the scaffolded
project validated; esbuild produced `bundle.js`, normalized manifest, and digest
files; the local dev runner executed the generated `pr.synchronize` fixture via
`@git-ai-project/workflows/testing` and printed the expected `summarize` step and
workflow log output. The smoke also uploads the generated bundle to a local fake
Git AI workflow API that serves `GET /api/workflows/capabilities`, verifies the
CLI sends the configured `X-API-Key`, bundle/source digests, base64 bundle
content, `bullmq` backend, activation flag, and `pr.synchronize` trigger, then
returns the normal `POST /api/workflows/upload` response shape. The same smoke
then exercises the management and post-upload loop against the fake API:
`list`, `approve`, `reject`, `activate`, `disable`, `rollback`, `archive`,
`restore`, `runtime-key rotate`, `runtime-key revoke`, `secrets set/list/delete`,
`notifications routes set/list/delete`, `trigger pr.synchronize`,
`backfill pr.synchronize`, `runs`, `logs`, `artifacts` list/fetch, `refresh`,
`restart --from-step`, and `cancel`.

## MVP Boundary

First implementation should ship:

- `init` - implemented;
- `validate` - implemented for manifest schema, `pr.synchronize` filters,
  runtime/backend, permission groups, network allowlist entries, secret names,
  positive limits, and exactly one default workflow export;
- `dev` - implemented with a generated Node/tsx runner using
  `@git-ai-project/workflows/testing`, plus polling `--watch` mode;
- `bundle` - implemented with esbuild-backed ESM bundling, digest generation,
  and bundle-size enforcement before upload;
- `upload` - implemented against `POST /api/workflows/upload`; it sends bundle
  bytes inline so API and worker containers do not need a shared local
  filesystem, and first checks SDK compatibility against
  `GET /api/workflows/capabilities`;
- `list` - implemented against `GET /api/workflows`;
- `approve` - implemented against `POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/approve`;
- `reject` - implemented against `POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/reject`;
- `activate` - implemented against `POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/activate`;
- `disable` - implemented against `POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/disable`;
- `rollback` - implemented against `POST /api/workflows/definitions/:workflowDefinitionId/deployments/:workflowDeploymentId/rollback`;
- `archive` - implemented against `POST /api/workflows/definitions/:workflowDefinitionId/archive`;
- `restore` - implemented against `POST /api/workflows/definitions/:workflowDefinitionId/restore`;
- `runs` - implemented against `GET /api/workflows/runs`;
- `inspect` - implemented against `GET /api/workflows/runs/:runId`;
- `logs` - implemented against `GET /api/workflows/runs/:runId/logs`.
- `artifacts` - implemented against run detail artifact metadata and
  `GET /api/workflows/runs/:runId/artifacts/:artifactId` for JSON content.
- `cancel`, `refresh`, and `restart` - implemented against workflow run
  lifecycle endpoints.
- `secrets list/set/delete` - implemented against workflow secret API endpoints.
- `notifications routes list/set/delete` - implemented against workflow
  notification route API endpoints.
- `trigger pr.synchronize` - implemented against
  `POST /api/workflows/triggers/pr.synchronize`; the server binds fixture events
  to the API key's organization before enqueueing dispatch.
- `backfill pr.synchronize` - implemented against
  `POST /api/workflows/triggers/pr.synchronize/backfill`; the server rebuilds
  historical `pr.synchronize` events from ClickHouse/Postgres and enqueues them
  through the normal workflow dispatch path.
