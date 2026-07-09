# Telemetry Examples

Telemetry metrics stored in `~/.git-ai/internal/metrics-db` use compact JSON:

```json
{"t": 1700000000, "e": 4, "v": {"0": 1700000000}, "a": {"0": "<git-ai version>"}}
```

- `t`: event Unix timestamp.
- `e`: event kind.
- `v`: event-specific values, keyed by numeric position.
- `a`: common attributes. Useful positions: `0` git-ai version, `1` repo URL, `2` author, `3` commit SHA, `4` base commit SHA, `5` branch, `20` tool, `21` model, `23` external session ID, `24` session ID, `25` trace ID.

Examples below were based on rows sampled from the local SQLite DB, but all string values and identifiers are anonymized.

## 1. `committed`

Source: local SQLite sample.

Emitted after a commit is processed.

```json
{
  "e": 1,
  "v": {
    "0": 0,
    "1": 2,
    "2": 10,
    "3": ["<aggregate>", "<tool>::<model>"],
    "5": [10, 10],
    "6": [10, 10],
    "10": 1700000000,
    "11": "<commit subject>",
    "12": null,
    "13": "<authorship note>",
    "14": "<diff hunks JSON>",
    "15": 1700000100,
    "16": 1700000200,
    "17": "<stable patch id>"
  },
  "a": {
    "0": "<git-ai version>",
    "1": "<repo URL>",
    "2": "<git author>",
    "3": "<commit SHA>",
    "4": "<base commit SHA>",
    "5": "<branch>"
  }
}
```

Key fields: human additions, git diff lines, tool/model breakdown, accepted AI lines, commit text, authorship note, hunks, author/commit timestamps, patch ID.

## 2. `agent_usage`

Source: local SQLite sample.

Emitted on AI checkpoint usage. The values object is intentionally empty; dimensions live in attributes.

```json
{
  "e": 2,
  "v": {},
  "a": {
    "0": "<git-ai version>",
    "1": "<repo URL>",
    "5": "<branch>",
    "20": "<tool>",
    "21": "<model>",
    "23": "<external session ID>",
    "24": "<session ID>"
  }
}
```

## 3. `install_hooks`

Source: local SQLite sample.

Emitted once per tool attempted by `git-ai install-hooks`.

```json
{
  "e": 3,
  "v": {
    "0": "<tool id>",
    "1": "<install status>",
    "2": null
  },
  "a": {"0": "<git-ai version>"}
}
```

Key fields: tool ID, install status, optional message.

## 4. `checkpoint`

Source: local SQLite sample.

Emitted per file in a checkpoint.

```json
{
  "e": 4,
  "v": {
    "0": 1700000000,
    "1": "<checkpoint kind>",
    "2": "<repo-relative file path>",
    "3": 3,
    "4": 0,
    "5": 3,
    "6": 0,
    "8": "<edit kind>",
    "9": "<checkpoint type>",
    "10": "{\"solver\":\"<solver>\",\"recovered_lines\":[1,2,3]}"
  },
  "a": {
    "0": "<git-ai version>",
    "1": "<repo URL>",
    "3": "<commit SHA>",
    "4": "<base commit SHA>",
    "5": "<branch>",
    "24": "<session ID>",
    "25": "<trace ID>"
  }
}
```

Key fields: checkpoint timestamp, checkpoint kind, file path, added/deleted lines, SLOC counts, optional external tool-use ID, edit kind, checkpoint type, recovery metadata.

## 5. `session_event`

Source: local SQLite sample.

Emitted from agent transcript events.

```json
{
  "e": 5,
  "v": {
    "0": {
      "type": "<event type>",
      "timestamp": "<ISO timestamp>",
      "payload": {
        "type": "<payload type>",
        "duration_ms": 1234,
        "turn_id": "<turn ID>",
        "last_agent_message": "<redacted>"
      }
    }
  },
  "a": {
    "0": "<git-ai version>",
    "1": "<repo URL>",
    "20": "<tool>",
    "23": "<external session ID>",
    "24": "<session ID>",
    "25": "<trace ID>"
  }
}
```

Optional values `1`, `2`, and `3` carry external event, parent event, and tool-use IDs when the transcript format provides them.

## 6. `otel_trace`

Source: local SQLite sample.

Emitted from Copilot OTEL trace SQLite data.

```json
{
  "e": 6,
  "v": {
    "0": {
      "span": {
        "name": "<span name>",
        "operation_name": "<operation>",
        "provider_name": "<provider>",
        "input_tokens": 1000,
        "output_tokens": 100,
        "ttft_ms": 123.0,
        "span_id": "<span ID>",
        "trace_id": "<provider trace ID>"
      },
      "attributes": "<redacted OTEL attributes>"
    },
    "1": "<external event/span ID>"
  },
  "a": {
    "0": "<git-ai version>",
    "20": "<tool>",
    "23": "<external session ID>",
    "24": "<session ID>",
    "25": "<trace ID>"
  }
}
```

Optional values `1`, `2`, and `3` carry external event, parent event, and tool-use IDs.

## 7. `rewrite_committed`

Source: no local row found with `event_kind = 7` in `~/.git-ai/internal/metrics-db`; example is from the schema in `src/daemon/rewrite_metrics.rs`.

Emitted after rewrite operations create replacement commits and authorship notes are migrated.

```json
{
  "e": 7,
  "v": {
    "0": 1,
    "1": 2,
    "2": 3,
    "3": ["<aggregate>"],
    "5": [1],
    "6": [1],
    "11": null,
    "12": null,
    "13": "<migrated authorship note>",
    "14": null,
    "15": "<rewrite operation>",
    "16": ["<original commit SHA>"]
  },
  "a": {
    "0": "<git-ai version>",
    "1": "<repo URL>",
    "3": "<new commit SHA>",
    "4": "<base commit SHA>",
    "5": "<branch>"
  }
}
```

Position `10` is intentionally not emitted for rewrite events.
