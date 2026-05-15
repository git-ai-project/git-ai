# CI Integration

Detects CI environments (GitHub Actions, GitLab CI) and computes attribution reports for pull request diffs.

## What it does

- Detects whether the current process is running inside a known CI provider
- Computes AI vs human attribution percentages for a range of commits
- Formats attribution reports as Markdown suitable for PR/MR comments
- Supports threshold-based exit codes (`--max-ai-percent`) for policy enforcement

## Key types

- `CiEnvironment` — detected CI context (provider, PR number, base/head refs)
- `AttributionReport` — per-file and aggregate AI% for a commit range

## Data flow

```
git notes --ref=ai  →  parse AuthorshipLog  →  aggregate stats  →  Markdown report
```

The module reads existing authorship notes (produced by the daemon or post-commit hook) and summarizes them — it never writes notes itself.
