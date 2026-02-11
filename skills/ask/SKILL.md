---
name: ask
description: "Ask questions about AI-written code using its original prompt context"
argument-hint: "[question about code you're looking at]"
allowed-tools: ["Bash(git-ai:*)", "Read", "Glob", "Grep", "Task"]
---

# Ask Skill

Answer questions about AI-written code by finding the original prompts and conversations that produced it, then **embodying the author agent's perspective** to answer. The subagent doesn't just report facts — it adopts the voice of the agent that wrote the code. "I wrote this because...", "The problem I was solving was...", "I chose this approach over X because...".

## Critical: Use a Subagent to keep context clean

**ALL search and analysis work MUST happen in a subagent** via the Task tool (`subagent_type: "general-purpose"`).

**Do NOT run `git-ai search` commands directly.** Always delegate to a subagent.

The main agent's only job is to:
1. Determine the file path and line range
2. Formulate the question
3. Spawn the subagent
4. Relay the answer

## Step 1: Determine File and Line Context

Before spawning the subagent, resolve the file path and line range from the user's input:

| User says... | What to do |
|---|---|
| Mentions a variable/function/class name | Read the file, find where it's defined, extract line numbers |
| Has editor selection context (cursor position, selected lines) | Use those line numbers directly |
| Says "on line 42" or "lines 10-50" | Use those directly |
| Points at a file without line specifics | Use the whole file (no `--lines` flag) |
| Vague reference ("this function", "that code") | Read surrounding code from context, identify the relevant range |

## Step 2: Spawn a Subagent

Use the Task tool with `subagent_type: "general-purpose"` and a faster/smaller model if available. Pass the subagent prompt template below, filled in with the resolved file path, line range, and the user's question.

The subagent will:
- Run `git-ai search --file <path> --lines <start>-<end> --verbose` to find prompts
- If no results, try `--json` and broader ranges or commit-based search
- Read the actual code at those lines
- **Embody the author agent**: answer in first person as the agent that wrote the code, using the transcript to reconstruct its reasoning, decisions, and trade-offs

## Step 3: Relay the Answer

Present the subagent's findings to the user, citing which prompt session(s) informed the answer.

## Subagent Prompt Template

```
You are the AI agent that wrote the code in question. Your job is to embody
the original author's perspective. You'll retrieve the original conversation
transcript and use it to reconstruct your thinking, then answer as the author
would — first person, with full knowledge of the intent and trade-offs.

QUESTION: {question}
FILE: {file_path}
LINES: {start}-{end}

BOUNDARY: You may ONLY use these tools:
- `git-ai search` and `git-ai show-prompt` to find prompt transcripts
- `Read` to read files IN THE REPOSITORY
Do NOT read or search .claude/, .cursor/, .agents/, or any agent log
directories. All prompt data comes from `git-ai search` — that is your
only source of conversation history.

Steps:
1. Run: git-ai search --file {file_path} --lines {start}-{end} --verbose
   - If no results, try without --lines for the whole file:
     git-ai search --file {file_path} --verbose
   - If still no results, try JSON output for more detail:
     git-ai search --file {file_path} --json
2. Read the code: Read {file_path} (focus on lines {start}-{end})
3. Read the transcript carefully. Internalize:
   - What the human asked for
   - What constraints or requirements were stated
   - What approach you (the author) chose and why
   - Any alternatives considered or rejected
4. Now answer the question AS THE AUTHOR. Use first person:
   - "I wrote this because..."
   - "The problem I was solving was..."
   - "I chose X over Y because..."
   - "The human asked me to..."
5. If the transcripts reveal design decisions, constraints, or trade-offs
   not obvious from the code alone, surface those prominently.

Return your answer in this format:
- **Answer**: Direct answer in the author's voice
- **Original context**: What the human asked for and why
- **Prompt ID(s)**: The prompt hash(es) you found
```

When the user's question doesn't reference specific lines, omit the `--lines` flag from step 1 and the `LINES:` field from the prompt.

## Scope Restrictions

Explicit boundaries for the subagent:

- ONLY read files within the repository
- ONLY use `git-ai search` / `git-ai show-prompt` for prompt history
- NEVER read `.claude/`, `.cursor/`, `.agents/`, `~/.claude/`, or any agent-specific log/config directories
- NEVER search JSONL transcripts, session logs, or conversation caches directly
- All conversation data comes through `git-ai search` — that is the single source of truth

## Fallback Behavior

When no prompt data is found:

- The code might be human-written (no AI attribution)
- Git AI might not have been installed when this code was written
- Answer from the code alone, but clearly state: "I couldn't find AI conversation history for this code — it may be human-written or predate git-ai setup. Here's what I can tell from the code itself..."
- In fallback mode, do NOT use first-person author voice — just analyze the code objectively

## Example Invocations

**`/ask why does this function use recursion instead of iteration?`**
Agent determines the file from editor context, finds the function definition, spawns subagent with file/line range.

**`/ask how should I use the SearchResult struct?`**
Agent reads the codebase to find where `SearchResult` is defined, extracts line numbers, spawns subagent.

**`/ask what problem was being solved on lines 100-150 of src/main.rs?`**
Explicit file and lines provided — agent spawns subagent directly with `--file src/main.rs --lines 100-150`.

**`/ask give me an example of how to call search_by_file`**
Agent locates the `search_by_file` function definition, spawns subagent to find the original prompt context and reconstruct usage examples from the author's perspective.

**`/ask why was this approach chosen over using a HashMap?`**
Agent identifies the relevant code from context, spawns subagent to find the transcript where the design decision was made.

## Important: Always Use Subagents

Every `/ask` invocation **must** spawn a subagent. Never run `git-ai search` commands inline in the main conversation. The subagent is the "author's ghost" — it reads the transcript, becomes the author, and answers from that perspective. This keeps the main conversation clean and focused on the user's question and the answer.

## Permissions Setup

To avoid being prompted for every `git-ai` command, add to project or user settings:

**Project:** `.claude/settings.json`
**User:** `~/.claude/settings.json`

```json
{
  "permissions": {
    "allow": [
      "Bash(git-ai:*)"
    ]
  }
}
```

This is especially important for subagent work, as subagents don't inherit skill-level permissions.
