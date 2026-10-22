# Attribution counts in commit stats

`git-ai stats <commit> --json` counts lines added by the commit's diff, rather
than the number of attribution records covering those lines. Repeated ranges,
sessions, or file records do not count an added line again. The same line number
in two different files represents two additions.

The headline buckets are exclusive:

- `human_additions`: added lines with explicit known-human (`h_`) attribution,
  including lines that also have AI attribution.
- `ai_additions` and `ai_accepted`: added lines with AI attribution and no explicit
  known-human attribution. These two fields retain the same value.
- `unknown_additions`: the remaining added lines.

Consequently, `human_additions + ai_additions + unknown_additions` equals
`git_diff_added_lines`. Known-human precedence is a reporting convention: it
does not assert that the human edited last, that no AI contributed to that line,
or that the human was its only author. An AI session's `human_author` metadata
alone is not a known-human line attestation.

## Tool/model coverage is not additive

Each `tool_model_breakdown` entry counts distinct qualifying AI-added lines for
that tool/model. Multiple sessions or legacy prompts resolving to the same key
are combined. Different keys can cover the same line, so their counts must not
be summed to obtain the headline AI total or displayed as exclusive shares.

For example, if model A covers lines 1–4 and model B covers lines 3–6, with no
known-human attribution, the headline AI count is 6 and each model's count is 4.
The original overlapping provenance is retained in the authorship note.

Known-human lines are excluded from each model's count as well as the headline
AI count. A model with no qualifying added lines is omitted. Existing metadata
resolution is unchanged: non-`h_` attestations still contribute to the AI
headline when their session or legacy prompt metadata is unavailable, but they
do not create an invented tool/model entry.

## Scope and compatibility

This counting rule does not change stored notes, blame attribution, file-ignore
selection, the diff used to find additions, or existing merge-commit handling.
Overlapping historical notes can therefore produce lower counts when inspected
again. Non-overlapping records retain their counts. The JSON field names and
types are unchanged; consumers must treat model counts as overlapping coverage.

The commit-level rule does not redefine the separate range-statistics metrics.

Telemetry continues to omit mock-model entries. Its aggregate excludes only
added lines attributed exclusively to mock models, so overlap with a real or
unresolved model does not remove that model's coverage. Mock-exclusive counts
are carried internally and do not add fields to the public JSON output.
