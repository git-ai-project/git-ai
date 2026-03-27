---
name: Demo test plan document
overview: Informal demo test plan for verifying the research metrics extension. Describes prompts to give Cursor, manual edits to make, and what to look for in the debug output and git notes.
todos:
  - id: run-steel-thread
    content: Walk through the 5-step demo scenario by hand in a scratch repo
    status: pending
  - id: verify-git-note
    content: Eyeball the git note JSON to confirm change_history, prompt_ids, and line ranges look right
    status: pending
isProject: false
---

# Demo Test Plan: Research Metrics Extension

## What This Is

An informal walkthrough to verify the research metrics extension works end-to-end. You do this by hand in a scratch project -- open it in Cursor, give prompts, make some manual edits, commit, and inspect the output. `debug_log` output (printed to stderr as `[git-ai] ...`) tells you what happened at each step, and the final git note tells you whether it all landed correctly.

Run with `GIT_AI_DEBUG=1` (or use a debug build) to see `debug_log` output.

---

## Part 1: Setup

1. Build the project: `cargo build` (in `nix develop` shell)
2. Create a new scratch project folder somewhere outside git-ai
3. `git init` it, set a user name/email
4. Run `git-ai install-hooks` so the proxy hooks are active
5. Create two starter files and make an initial commit (use `git-og` for this commit so git-ai doesn't fire):

**math.py**

```python
def add(a, b):
    return a + b

def subtract(a, b):
    return a - b
```

**utils.py**

```python
def format_number(n):
    return str(n)
```

---

## Part 2: The Walkthrough

### Step A: First AI prompt -- multi-file edit

Open Cursor in the scratch project. Give it this prompt:

> "Add a multiply function to math.py and add a format_currency function to utils.py"

Let it edit both files. After Cursor finishes, a checkpoint fires automatically.

**What to look for in `[git-ai]` debug output:**

- `Checkpoint created: kind=AiAgent, prompt_id=Some("...")` -- should have a prompt_id (the bubble_id of your user message)
- Two file entries: `math.py` with added_ranges for the new lines, `utils.py` with added_ranges
- `Working log written: 1 checkpoints`

**Quick sanity check:** Run `git-ai status` -- should show 1 AI checkpoint with some additions and 0 deletions.

---

### Step B: Human edit -- manually change 1 file

Now YOU (not the AI) manually edit `math.py`. Add input validation to the multiply function:

```python
def multiply(a, b):
    if not isinstance(a, (int, float)) or not isinstance(b, (int, float)):
        raise TypeError("Arguments must be numbers")
    return a * b
```

Save the file. The pre-commit hook will pick this up as a human checkpoint when you eventually commit, but you can also trigger one explicitly with `git-ai checkpoint` to see the log now.

**What to look for:**

- `Checkpoint created: kind=Human, prompt_id=None` -- no prompt_id because it's a human edit
- File entry for `math.py` with both added_ranges (the new validation lines) and deleted_ranges (the original one-liner body)
- `Working log written: 2 checkpoints`

**Sanity check:** `git-ai status` -- 2 checkpoints now, one AI and one human.

---

### Step C: Second AI prompt -- overlapping file

Give Cursor another prompt, targeting the same file the human just edited:

> "Add a divide function to math.py with error handling for division by zero"

Let it edit. Another checkpoint fires.

**What to look for:**

- `Checkpoint created: kind=AiAgent, prompt_id=Some("...")` -- prompt_id should be the bubble_id of THIS prompt, not the first one
- File entry for `math.py` with added_ranges covering the new divide function lines
- `Working log written: 3 checkpoints`

**Key thing to verify:** The prompt_id on this checkpoint is different from Step A's prompt_id. This proves prompt-level linking works -- each checkpoint points to the specific user message that triggered it, not just the conversation.

---

### Step D: Plan conversation -- no code changes

Give Cursor a question that doesn't result in any code edits:

> "What's the best way to add logging to this project? Don't make any changes, just describe a plan."

**Current expected behavior:** This likely produces no checkpoint (no file diff = nothing to record). This is the gap we'll fill later.

**Future expected behavior:** The conversation should still appear in the git note metadata under `conversations` / `prompts`, even with zero `change_history` entries. Messages should have their bubble_ids preserved.

---

### Step E: Commit and inspect

Stage everything and commit (through the git-ai proxy, so the post-commit hook fires):

```
git add -A
git commit -m "Add math operations and currency formatter"
```

**What to look for in `[git-ai]` debug output during commit:**

- `change_history built: 3 entries` (one per checkpoint from steps A, B, C)
- Entry 0: kind=AiAgent, prompt_id from Step A, files=[math.py, utils.py]
- Entry 1: kind=Human, prompt_id=None, files=[math.py]
- Entry 2: kind=AiAgent, prompt_id from Step C, files=[math.py]
- `Git note written for commit <sha>. Note size: NNNN bytes`

**Inspect the final git note:**

```
git-og notes --ref=ai show HEAD
```

This dumps the raw note: attestation text lines above `---`, then pretty-printed JSON metadata below.

To see just the JSON:

```
git-og notes --ref=ai show HEAD | sed -n '/^---$/,$ p' | tail -n +2 | python3 -m json.tool
```

---

## Part 3: What the Git Note Should Look Like

Eyeball the JSON metadata section for these things:

### schema_version

Should be `"authorship/4.0.0"` (bumped from 3.0.0).

### change_history

Array of 3 entries in chronological order:

- **Entry 0** (Step A): kind=`"ai_agent"`, prompt_id=some bubble_id, two files (`math.py` and `utils.py`), each with `added_lines` ranges, model=whatever Cursor used
- **Entry 1** (Step B): kind=`"human"`, prompt_id=null, one file (`math.py`), has both `added_lines` and `deleted_lines`
- **Entry 2** (Step C): kind=`"ai_agent"`, prompt_id=a different bubble_id than entry 0, one file (`math.py`), `added_lines` for the divide function

### prompts

Should have an entry (keyed by a short hash) with:

- `messages` array where each message has an `id` field (the bubble_ids)
- `agent_id.model` set to the model Cursor used
- `total_additions` / `total_deletions` reflecting AI-only contributions

### Attestation text (above ---)

Lines like `math.py <hash> 7-10,12-16` showing which line ranges are AI-attributed. The exact ranges depend on the final content, but AI-written lines (multiply body, divide function, format_currency) should be attributed.

---

## Part 4: Also Check These

- `**git-ai blame math.py`** -- should show AI attribution markers on the AI-written lines, human markers on the validation lines you added manually
- `**git-ai diff HEAD~1..HEAD --json`** -- JSON output should include annotations mapping line ranges to session hashes
- `**git-ai status --json**` (before committing, if you want to check intermediate state) -- shows checkpoint list with tool/model info

---

## Part 5: Future -- Plan Conversation Tracking

Step D above is a placeholder for future work. When implemented:

- A checkpoint with no file diffs should still record the conversation in the note metadata
- The `conversations` / `prompts` section should include the planning conversation with its message IDs
- `change_history` should have no entry for it (no code changed)
- This enables research analysis of conversations that informed the developer's thinking without producing direct code output

