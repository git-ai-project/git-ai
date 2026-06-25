# Metrics DB Event Metadata Columns Plan

## Goal

Add nullable event metadata columns to the local metrics SQLite table, populate them for all newly inserted metric rows, and asynchronously backfill existing rows. Keep upload behavior unchanged and keep malformed legacy rows representable by leaving the new columns `NULL`.

The metadata columns are:

- `event_ts`
- `event_kind`
- `trace_id`
- `session_id`
- `parent_session_id`
- `tool`
- `external_session_id`
- `external_parent_session_id`
- `external_event_id`
- `external_parent_event_id`
- `external_tool_use_id`

## Current State

- `src/metrics/db.rs` owns metrics DB schema versioning with `SCHEMA_VERSION` and `MIGRATIONS`.
- The `metrics` table stores canonical event payloads in `event_json`.
- Migration 2 -> 3 already uses helper logic before SQL to add retry columns idempotently with `add_column_if_missing`.
- New rows are inserted through `insert_events()` and `insert_events_with_delivered_ts()`.
- History and retention currently reparse `event_json` for every row to find timestamp and event id.
- Telemetry writes enter the DB asynchronously through `DaemonTelemetryWorkerHandle::submit_telemetry()` and synchronously through `submit_telemetry_sync()`.
- `EventAttributes` already standardizes `trace_id`, `session_id`, `parent_session_id`, `tool`, `external_session_id`, and `external_parent_session_id`.
- `external_event_id`, `external_parent_event_id`, and `external_tool_use_id` are still event-specific values for `session_event` and `otel_trace`; checkpoint stores `external_tool_use_id` in checkpoint values.

## Requirements

1. Add a metrics DB migration that creates all requested metadata columns as nullable columns.
2. Do not change metric event schemas or `EventAttributes`; this is a storage-only denormalization.
3. New metrics inserts must set every available metadata column from valid metric JSON.
4. Malformed or schema-incomplete metric JSON must still insert; unavailable columns remain `NULL`.
5. Existing metric rows must be backfilled asynchronously, not as blocking migration work.
6. Backfill must be idempotent and resumable.
7. Reads must remain correct before, during, and after backfill.
8. Avoid duplicating JSON extraction logic.

## Design

### Schema

- Bump `SCHEMA_VERSION` from `3` to `4`.
- Add migration 3 -> 4:
  - `event_ts INTEGER DEFAULT NULL`
  - `event_kind INTEGER DEFAULT NULL`
  - requested identifier columns as nullable `TEXT`
  - `metrics_event_ts_kind` index on `(event_ts, event_kind, id)` for rows where both metadata columns are non-null.
  - `metrics_session_kind_ts` index on `(session_id, event_kind, event_ts, id)` for rows where session, kind, and timestamp metadata are non-null.
  - `metrics_parent_session_kind_ts` index on `(parent_session_id, event_kind, event_ts, id)` for rows where parent session, kind, and timestamp metadata are non-null.
- Use a dedicated helper `add_event_metadata_columns()` from `apply_migration(3)` so partially applied/concurrent states are handled the same way as the retry-column migration.

### Metadata Sources

Keep `EventAttributes` unchanged. The DB extractor reads existing common attributes from `a`:

- `trace_id`
- `session_id`
- `parent_session_id`
- `tool`
- `external_session_id`
- `external_parent_session_id`

The external event and tool-use IDs are event-specific, so the DB extractor reads them from existing `v` value positions:

- event 4 checkpoint value position 7 -> `external_tool_use_id`
- event 5 session_event value positions 1/2/3 -> external event/parent/tool-use IDs
- event 6 otel_trace value positions 1/2/3 -> external event/parent/tool-use IDs

### Shared Metadata Extraction

Introduce one private helper:

```rust
fn extract_metric_event_metadata(event_json: &str) -> Option<MetricEventMetadata>
```

It parses only the compact top-level fields needed from metrics JSON:

- `t` -> `event_ts: u32`
- `e` -> `event_kind: u16`
- `a` attributes:
  - `trace_id`
  - `session_id`
  - `parent_session_id`
  - `tool`
  - `external_session_id`
  - `external_parent_session_id`
- event-specific values:
  - event 4 checkpoint value position 7 -> `external_tool_use_id`
  - event 5 session_event value positions 1/2/3 -> external event/parent/tool-use IDs
  - event 6 otel_trace value positions 1/2/3 -> external event/parent/tool-use IDs

The helper returns `None` if the top-level timestamp or event kind is missing, null, negative, not an integer, or out of range. Optional string identifiers are independently extracted when valid strings are present. This helper is the only code path that interprets DB metadata from `event_json`.

### Inserts

Change `insert_events_with_delivered_ts()` to parse each event once and insert:

- `event_json`
- `event_ts`
- `event_kind`
- all available identifier columns
- optional `delivered_ts`

Malformed/incomplete events are inserted with `NULL` metadata columns. If timestamp/kind are valid but optional IDs are absent, only the absent IDs remain `NULL`. This preserves offline retry and manual flush behavior for invalid rows.

### Reads Before/During/After Backfill

Use metadata columns when present; fall back to `event_json` parsing when either timestamp or event kind is null.

- `get_metric_history()`:
  - SQL prefilter can use `event_ts` and `event_kind` when present.
  - Fallback parsing remains necessary for legacy rows that have not been backfilled yet.
  - Returned `MetricHistoryRecord` still contains the full parsed `MetricEvent`.
- Retention pruning:
  - Prefer `event_ts`.
  - Fall back to JSON timestamp.
  - For malformed delivered rows, keep existing delivered timestamp fallback.

This keeps behavior correct while async backfill is still pending.

### Async Backfill

Add `MetricsDatabase::backfill_event_metadata_batch(limit)`:

- Select rows where `event_ts IS NULL OR event_kind IS NULL`, ordered by `id`.
- Parse metadata using `extract_metric_event_metadata`.
- Update columns where valid metadata is available.
- Leave invalid or missing metadata null.
- Return a summary `{ scanned, updated }`.

Add `MetricsDatabase::backfill_event_metadata()` as a cursor-based loop over batches. It advances by row id so malformed legacy rows that must remain null cannot trap the loop, and it stops when fewer than `limit` rows were scanned.

Triggering:

- Spawn the backfill from `spawn_telemetry_worker()` so daemon startup eventually fixes prior rows even if no new metric events arrive.
- Run in `spawn_blocking` because it uses rusqlite and the global DB mutex.
- Lock the global DB for one bounded batch at a time so ordinary metric operations can interleave with large backfills.

Backfill can safely race with new inserts because:

- New inserts write metadata immediately.
- Backfill only scans rows whose required timestamp/kind metadata is still null.
- Updates are idempotent.

### Tests First

Add RED unit tests in `src/metrics/db.rs`:

1. Fresh schema is version 4 and includes all nullable metadata columns.
2. Version 3 DB migrates to version 4 without backfilling rows synchronously.
3. New valid inserts populate `event_ts`, `event_kind`, and existing common identifiers from attributes, including delivered rows.
4. New inserts populate event-specific external event IDs/tool-use IDs from session_event, otel_trace, and checkpoint values.
5. Malformed/incomplete inserts keep metadata null.
6. Backfill updates valid legacy rows and leaves invalid rows null.
7. History reads legacy rows correctly before backfill and uses the same records after backfill.
8. Retention prunes old rows using `event_ts` when present.

## Review Passes

### Pass 1: DRY

Metadata extraction must live in exactly one helper. Inserts, backfill, and fallback read paths should call that helper rather than each defining partial JSON structs. Common attribute position constants and event-specific value position constants should be reused instead of numeric string literals in extraction code.

### Pass 2: Migration Safety

The migration should be additive, nullable, and idempotent. It must not rewrite all existing rows while holding schema initialization, and it must tolerate partially applied columns.

### Pass 3: Runtime Safety

New inserts continue to accept bad JSON because upload/error handling already accounts for invalid records. Backfill must be best-effort and must not prevent the daemon from starting or metrics from flushing.

### Pass 4: Performance

The new columns should let common history/retention paths avoid JSON parsing for backfilled/new rows, while preserving fallback correctness for old rows. Backfill should operate in bounded batches. Session-scoped indexes should put equality predicates first (`session_id` or `parent_session_id`, then `event_kind`) and timestamp after them so filtered metric queries can use the same index for timestamp ranges and stable row ordering.

### Pass 5: Verification

Use targeted unit tests during TDD, then run `task fmt`, `task lint`, and targeted/full tests before opening the PR.
