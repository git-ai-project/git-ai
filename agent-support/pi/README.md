# Pi support

`git-ai install-hooks` installs a managed Pi extension at:

- `~/.pi/agent/extensions/git-ai.ts`

That extension emits canonical Pi `before_edit` / `after_edit` checkpoint payloads and runs:

```bash
git-ai checkpoint pi --hook-input stdin
```

## Override file

Optional user-owned override file:

- `~/.pi/agent/git-ai.override.json`

`install-hooks` does not rewrite that file.

## Override contract

```json
{
  "version": 1,
  "tools": {
    "edit": {
      "kind": "mutating",
      "canonical": "edit",
      "filepath_fields": ["path"]
    },
    "bash": {
      "kind": "ignore"
    }
  }
}
```

### Rules

- tool key = raw Pi tool name
- `kind: "mutating"` requires:
  - `canonical`: `edit|write|replace|rename`
  - `filepath_fields`: top-level input field names used to extract touched paths
- `kind: "ignore"` explicitly disables tracking for that raw tool
- override entry replaces the built-in policy for the same raw tool
- missing override file means built-ins only

## Built-in defaults

Without an override file, the managed Pi extension only treats these as mutating:

- `edit` -> canonical `edit`, filepath field `path`
- `write` -> canonical `write`, filepath field `path`

## Example: add Serena mutators

```json
{
  "version": 1,
  "tools": {
    "serena_replace_symbol_body": {
      "kind": "mutating",
      "canonical": "replace",
      "filepath_fields": ["relative_path"]
    },
    "serena_insert_after_symbol": {
      "kind": "mutating",
      "canonical": "edit",
      "filepath_fields": ["relative_path"]
    },
    "serena_insert_before_symbol": {
      "kind": "mutating",
      "canonical": "edit",
      "filepath_fields": ["relative_path"]
    },
    "serena_replace_content": {
      "kind": "mutating",
      "canonical": "replace",
      "filepath_fields": ["relative_path"]
    },
    "serena_rename_symbol": {
      "kind": "mutating",
      "canonical": "rename",
      "filepath_fields": ["relative_path"]
    }
  }
}
```

## Troubleshooting

Check generated extension:

```bash
grep -n "checkpoint', 'pi'\|git-ai.override.json" ~/.pi/agent/extensions/git-ai.ts
```

Check tracked checkpoints in a repo:

```bash
cat .git/ai/working_logs/*/checkpoints.jsonl
```

Check authored note on the latest commit:

```bash
git notes --ref=ai show HEAD
```
