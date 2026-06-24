# Bash Attribution Recovery Scenarios

## Manual Installed-Build Matrix

| Scenario | Expected result |
| --- | --- |
| Bash hook from worktree A writes a new file in sibling worktree B, then commit in B | New file lines are attributed to the agent session |
| Same cross-worktree bash write, then a later human-created file is committed in B | Bash-created file is attributed to the agent; later human file remains human/untracked |
| Same file: bash writes an unknown line, then a human appends after the bash post hook and before commit | Do not attribute the later human line to AI; with mtime-only recovery this may conservatively leave both lines human/untracked |
| Multiple overlapping agent sessions with nearby timestamps | Attribute to the best matching session; prefer evidence tied to the same directory/session when available |
| Rebase/cherry-pick/rewrite paths | Do not run mtime recovery during rewrite-note regeneration unless explicitly proven safe |

## PR #1625 Installed-Build Results

Tested from local branch `pr-1625` after installing with `task dev`.

| Scenario | Observed result | Status |
| --- | --- | --- |
| Cross-worktree bash write | `generated.txt` was blamed to `codex` | Working |
| Later human-created separate file after bash post-hook | `generated.txt` was blamed to `codex`, but `manual.txt` was also blamed to `codex` | Not working: over-attribution |
| Same-file human append after bash post-hook | Both the bash-created line and later human-appended line were blamed to `codex` | Not working: over-attribution |
| Two nearby Codex sessions writing separate files | Authorship note mapped `a.txt` to session A and `b.txt` to session B | Working |

### PR #1625 Takeaway

PR #1625 fixes Sasha's core cross-worktree miss, but it reproduces the main risk Sasha called out: a file modified after the bash post-hook and before commit can be swept into the bash session if its mtime lands in the recovery window.

The later-human separate-file case is the clearest blocker because no same-file ambiguity is involved: the file was created after the bash session, yet it was still attributed to Codex.

## Notes

- The core regression from Sasha is the first scenario: normal bash checkpoint scanning only looks in the originating repo directory, so sibling worktree writes can be missed.
- The main safety risk is over-attribution when file mtimes are updated after a bash session by later human edits.
- The same-file mixed edit case needs content/snapshot-based evidence to recover the AI subset safely.

## PR #1644 Ranking Rules

PR #1644 keeps the PR #1625 mtime recovery window, but changes which bash candidate is selected when several sessions are close enough to explain the same file timestamp.

The intended ranking order is:

1. Prefer a bash session ID that is already represented elsewhere in the commit.
2. If more than one commit session matches, choose the closest matching bash call by time.
3. If no candidate session is already represented in the commit, prefer a candidate whose recorded workdir contains the file being recovered.
4. If neither of the stronger signals applies, choose the closest matching bash call by time within the 3-second recovery window.

Tie-breakers after those signals keep the previous behavior: prefer completed bash calls, then calls with a captured command, then the newest database row.

### PR #1644 Expected Results

| Scenario | Expected result | Coverage |
| --- | --- | --- |
| Commit already contains attributed lines from session A, and an unknown file timestamp is closer to unrelated session B | Unknown lines are attributed to session A if session A is still inside the recovery window | `bash_candidate_ranking_prefers_session_already_in_commit_then_time` |
| Commit already contains attributed lines from sessions A and B | Unknown lines are attributed to the closer of A or B | `bash_candidate_ranking_prefers_session_already_in_commit_then_time` |
| No candidate session is already present in the commit, and one candidate workdir contains the target file | Unknown lines are attributed to the containing-workdir candidate, even if another candidate is slightly closer by time | `bash_candidate_ranking_prefers_parent_workdir_when_no_session_matches_commit` |
| No candidate session is in the commit and no candidate workdir contains the target file | Unknown lines are attributed to the closest candidate by time within the 3-second window | `bash_candidate_ranking_falls_back_to_closest_time` |
| Candidate workdir or target path includes a platform-level symlink, such as macOS `/var` to `/private/var` | Workdir containment is compared after canonicalizing the file path or its existing parent directory | `bash_candidate_ranking_prefers_parent_workdir_when_no_session_matches_commit` |

### PR #1644 Local Verification

Run these before merging changes to the ranking logic:

```bash
task fmt
task lint
task test TEST_FILTER=bash_candidate_ranking NO_CAPTURE=true
task test TEST_FILTER=bash_attribution NO_CAPTURE=true
task dev
```

The focused ranking test verifies the candidate ordering directly. The broader `bash_attribution` filter verifies the mtime recovery path through the integration test harness. `task dev` verifies the debug build can be installed locally for manual end-to-end testing.

### Remaining Known Gaps

- CWD-not-a-repo recording is intentionally out of scope for PR #1644.
- Same-file human edits after a bash post-hook can still require content-based evidence to split AI and human lines safely.
- The 3-second mtime window is unchanged; PR #1644 reduces cross-session misattribution inside that window, but does not replace mtime matching with content matching.
