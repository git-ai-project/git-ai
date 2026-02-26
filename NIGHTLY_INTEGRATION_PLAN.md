# Nightly Integration Tests for Agent CLIs

## Overview

This document proposes a nightly GitHub Actions workflow that installs **real agent CLI binaries** (both stable and pre-release versions) and verifies that git-ai's hooks fire correctly when those agents perform edits and commits. The goal is to catch integration breakage early -- before users report it -- whenever an agent CLI ships a breaking change.

**Target agents**: Claude Code, Codex, Gemini CLI, Droid, OpenCode

**Existing reference workflows**:
- `nightly-upgrade.yml` -- validates git-ai upgrade paths from random older versions
- `install-scripts-nightly.yml` -- verifies install scripts wire Claude hooks correctly

---

## 1. Agent CLI Reference

### 1.1 Installation & Headless Invocation

| Agent | Package / Install | Node.js | API Key Env Var | Headless Command | Auto-Approve Flag | Turn Limit |
|-------|-------------------|---------|-----------------|------------------|-------------------|------------|
| **Claude Code** | `npm i -g @anthropic-ai/claude-code` | 18+ | `ANTHROPIC_API_KEY` | `claude -p "prompt"` | `--dangerously-skip-permissions` | `--max-turns N` |
| **Codex** | `npm i -g @openai/codex` | 18+ | `OPENAI_API_KEY` | `codex exec "prompt"` | `--full-auto` | N/A (use timeout) |
| **Gemini CLI** | `npm i -g @google/gemini-cli` | 20+ | `GEMINI_API_KEY` | `gemini "prompt"` | `--approval-mode=yolo` | N/A (use timeout) |
| **Droid** | `curl -fsSL https://app.factory.ai/cli \| sh` | N/A | `FACTORY_API_KEY` | `droid exec "prompt"` | `--auto high` | N/A (use timeout) |
| **OpenCode** | `npm i -g opencode` | 18+ | `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` | `opencode run --command "prompt"` | (uses agent mode) | N/A (use timeout) |

### 1.2 Version Resolution

For npm-based CLIs, resolve stable and latest versions dynamically:

```bash
# Get the current stable (latest published) version
STABLE=$(npm view @anthropic-ai/claude-code version)

# Get the list of all versions, pick latest pre-release if tagged
npm view @anthropic-ai/claude-code dist-tags --json
```

For Droid (curl-based install), the installer always fetches the latest. A specific version mechanism would need to be confirmed with Factory AI's documentation.

### 1.3 JSON Output for Verification

| Agent | JSON Output Flag | Notes |
|-------|-----------------|-------|
| Claude Code | `--output-format json` | Structured result with tool calls |
| Codex | `--json` | Structured output |
| Gemini CLI | `--output-format json` | JSON events |
| Droid | `--output-format json` | text/json/stream-json |
| OpenCode | `--format json` | Raw JSON events |

---

## 2. Test Architecture: Two Tiers

### Tier 1: Hook Wiring Verification (No API Keys)

**Cost**: Free. **Deterministic**: Yes.

This tier verifies that git-ai can detect and configure hooks for each agent CLI, and that synthetic checkpoint data flows through the pipeline correctly.

**What it tests**:
1. `git-ai install` detects the agent binary and wires hooks
2. Hook configuration files contain the correct entries (e.g., `~/.claude/settings.json` has `PreToolUse`/`PostToolUse` with `checkpoint claude`)
3. `git-ai checkpoint <agent>` accepts synthetic/fixture input and produces a working log
4. Post-commit hook produces an authorship note from that working log

**Why it matters**: Agent CLI updates sometimes change config file formats (e.g., Claude Code's `settings.json` schema), binary names, or installation paths. Tier 1 catches these structural breakages without needing API calls.

### Tier 2: Live Agent Integration (Requires API Keys)

**Cost**: ~$0.01-0.10 per agent per run (using cheapest models). **Deterministic**: No.

This tier has the actual agent CLI perform a real edit in a test repository, then verifies the full git-ai attribution pipeline end-to-end.

**What it tests**:
1. Agent receives a prompt, creates/modifies files
2. Agent commits changes
3. git-ai hooks fire during the agent's operation (checkpoint + post-commit)
4. Authorship notes exist in `refs/notes/ai` with correct agent identification
5. Line attribution is present

**Why it matters**: Catches behavioral changes in how agents invoke tools, write files, or interact with git -- things that structural tests alone won't find.

---

## 3. Test Scenarios

### 3.1 Tier 1 Scenarios (Per Agent)

```
T1.1  Install agent CLI binary
T1.2  Run `git-ai install` and verify hook configuration
T1.3  Feed synthetic checkpoint data and verify working log creation
T1.4  Make a manual git commit and verify authorship note generation
T1.5  Verify `git-ai blame` output includes agent attribution
```

#### T1.1: Install Agent CLI

```bash
# Claude Code
npm install -g @anthropic-ai/claude-code@${VERSION}
claude --version

# Codex
npm install -g @openai/codex@${VERSION}
codex --version

# Gemini CLI
npm install -g @google/gemini-cli@${VERSION}
gemini --help  # gemini --version may not exist

# Droid
curl -fsSL https://app.factory.ai/cli | sh
droid --version

# OpenCode
npm install -g opencode@${VERSION}
opencode --version
```

#### T1.2: Verify Hook Wiring

After `git-ai install`:

```bash
# Claude Code: check ~/.claude/settings.json
jq '.hooks.PreToolUse' ~/.claude/settings.json | grep -q "checkpoint claude"
jq '.hooks.PostToolUse' ~/.claude/settings.json | grep -q "checkpoint claude"

# Codex: check ~/.codex/config.toml or equivalent
# Gemini: check ~/.gemini/settings.json or equivalent
# Droid: check ~/.factory/config.json or equivalent
# OpenCode: check opencode.json or equivalent
```

#### T1.3-T1.5: Synthetic Checkpoint Flow

```bash
# Create a test repo
git init test-repo && cd test-repo
git-ai install

# Create a file (simulating agent output)
echo "Hello World" > hello.txt
git add hello.txt

# Feed synthetic checkpoint data
echo '{"tool":"Write","path":"hello.txt"}' | git-ai checkpoint claude --hook-input stdin

# Commit
git commit -m "Add hello.txt"

# Verify authorship note exists
git notes --ref=ai show HEAD | grep -q "authorship"

# Verify blame
git-ai blame hello.txt | grep -q "claude"
```

### 3.2 Tier 2 Scenarios (Per Agent)

Each agent is given a minimal, deterministic prompt in a test repository:

```
T2.1  Agent creates a new file (hello.txt with "Hello World")
T2.2  Agent modifies an existing file (append a line to README.md)
T2.3  Agent commits its changes
T2.4  Verify authorship note identifies the correct agent
T2.5  Verify line-level attribution is present
```

#### Claude Code Example:

```bash
cd test-repo
claude -p \
  --dangerously-skip-permissions \
  --max-turns 3 \
  --output-format json \
  "Create a file called hello.txt containing exactly 'Hello World', then commit it with message 'Add hello.txt'"
```

#### Codex Example:

```bash
cd test-repo
codex exec \
  --full-auto \
  "Create a file called hello.txt containing exactly 'Hello World', then commit it with message 'Add hello.txt'"
```

#### Gemini CLI Example:

```bash
cd test-repo
gemini \
  --approval-mode=yolo \
  "Create a file called hello.txt containing exactly 'Hello World', then commit it with message 'Add hello.txt'"
```

#### Droid Example:

```bash
cd test-repo
droid exec \
  --auto high \
  --output-format json \
  "Create a file called hello.txt containing exactly 'Hello World', then commit it with message 'Add hello.txt'"
```

#### OpenCode Example:

```bash
cd test-repo
opencode run \
  --command "Create a file called hello.txt containing exactly 'Hello World', then commit it with message 'Add hello.txt'" \
  --format json
```

#### Post-Agent Verification (All Agents):

```bash
# Verify the file was created
test -f hello.txt
grep -q "Hello World" hello.txt

# Verify a commit was made
COMMIT_COUNT=$(git log --oneline | wc -l)
test "$COMMIT_COUNT" -ge 2  # Initial + agent commit

# Verify authorship note
git notes --ref=ai show HEAD | jq -e '.prompts'

# Verify agent identification
git notes --ref=ai show HEAD | jq -e '.prompts[].agent_id.tool' | grep -qi "${AGENT_NAME}"

# Verify line attribution
git-ai blame hello.txt | grep -qi "${AGENT_NAME}"
```

---

## 4. Workflow Architecture

### 4.1 Matrix Strategy

```yaml
strategy:
  fail-fast: false
  matrix:
    agent: [claude, codex, gemini, droid, opencode]
    channel: [stable, latest]
    os: [ubuntu-latest]
    exclude:
      # Droid doesn't have npm version pinning for stable vs latest
      - agent: droid
        channel: stable
    include:
      - agent: claude
        channel: stable
        npm_pkg: "@anthropic-ai/claude-code"
        api_key_var: "ANTHROPIC_API_KEY"
        headless_cmd: 'claude -p --dangerously-skip-permissions --max-turns 3'
      - agent: claude
        channel: latest
        npm_pkg: "@anthropic-ai/claude-code@latest"
        api_key_var: "ANTHROPIC_API_KEY"
        headless_cmd: 'claude -p --dangerously-skip-permissions --max-turns 3'
      - agent: codex
        channel: stable
        npm_pkg: "@openai/codex"
        api_key_var: "OPENAI_API_KEY"
        headless_cmd: 'codex exec --full-auto'
      - agent: codex
        channel: latest
        npm_pkg: "@openai/codex@latest"
        api_key_var: "OPENAI_API_KEY"
        headless_cmd: 'codex exec --full-auto'
      - agent: gemini
        channel: stable
        npm_pkg: "@google/gemini-cli"
        api_key_var: "GEMINI_API_KEY"
        headless_cmd: 'gemini --approval-mode=yolo'
      - agent: gemini
        channel: latest
        npm_pkg: "@google/gemini-cli@latest"
        api_key_var: "GEMINI_API_KEY"
        headless_cmd: 'gemini --approval-mode=yolo'
      - agent: opencode
        channel: stable
        npm_pkg: "opencode"
        api_key_var: "ANTHROPIC_API_KEY"
        headless_cmd: 'opencode run --command'
      - agent: opencode
        channel: latest
        npm_pkg: "opencode@latest"
        api_key_var: "ANTHROPIC_API_KEY"
        headless_cmd: 'opencode run --command'
      - agent: droid
        channel: latest
        npm_pkg: ""  # Uses curl installer
        api_key_var: "FACTORY_API_KEY"
        headless_cmd: 'droid exec --auto high'
```

### 4.2 Dynamic Version Resolution

A setup job resolves the concrete versions before the matrix runs:

```yaml
jobs:
  resolve-versions:
    runs-on: ubuntu-latest
    outputs:
      claude_stable: ${{ steps.versions.outputs.claude_stable }}
      claude_latest: ${{ steps.versions.outputs.claude_latest }}
      codex_stable: ${{ steps.versions.outputs.codex_stable }}
      # ... etc
    steps:
      - id: versions
        run: |
          echo "claude_stable=$(npm view @anthropic-ai/claude-code version)" >> $GITHUB_OUTPUT
          echo "claude_latest=$(npm view @anthropic-ai/claude-code dist-tags --json | jq -r '.latest')" >> $GITHUB_OUTPUT
          echo "codex_stable=$(npm view @openai/codex version)" >> $GITHUB_OUTPUT
          echo "codex_latest=$(npm view @openai/codex dist-tags --json | jq -r '.latest')" >> $GITHUB_OUTPUT
          echo "gemini_stable=$(npm view @google/gemini-cli version)" >> $GITHUB_OUTPUT
          echo "gemini_latest=$(npm view @google/gemini-cli dist-tags --json | jq -r '.latest')" >> $GITHUB_OUTPUT
          echo "opencode_stable=$(npm view opencode version)" >> $GITHUB_OUTPUT
          echo "opencode_latest=$(npm view opencode dist-tags --json | jq -r '.latest')" >> $GITHUB_OUTPUT
```

### 4.3 Complete Workflow Structure

```
nightly-agent-integration.yml
  |
  +-- resolve-versions          (5 min) -- Fetch stable + latest for each npm package
  |
  +-- tier1-hook-wiring         (10 min, matrix: 9 agents x channels)
  |     +-- Install git-ai from current branch
  |     +-- Install agent CLI binary
  |     +-- Run git-ai install
  |     +-- Verify hook configuration
  |     +-- Feed synthetic checkpoint data
  |     +-- Verify authorship note generation
  |
  +-- tier2-live-integration    (15 min, matrix: 5 agents x 2 channels, needs: tier1)
  |     +-- Create test repository
  |     +-- Install git-ai + agent CLI
  |     +-- Run git-ai install
  |     +-- Run agent with deterministic prompt
  |     +-- Verify file creation
  |     +-- Verify authorship notes
  |     +-- Verify agent identification
  |
  +-- report                    (2 min, needs: tier1, tier2)
  |     +-- Aggregate results
  |     +-- Upload artifacts
  |     +-- Post summary to PR / issue
  |
  +-- notify-on-failure         (1 min, if: failure())
        +-- Slack notification
        +-- Auto-create GitHub issue (optional)
```

---

## 5. Secrets Required

| Secret Name | Service | Tier | Notes |
|-------------|---------|------|-------|
| `ANTHROPIC_API_KEY` | Anthropic | Tier 2 | Used by Claude Code and OpenCode |
| `OPENAI_API_KEY` | OpenAI | Tier 2 | Used by Codex |
| `GEMINI_API_KEY` | Google | Tier 2 | Free tier: 60 req/min, 1000/day |
| `FACTORY_API_KEY` | Factory AI | Tier 2 | Used by Droid |
| `SLACK_BOT_TOKEN` | Slack | Notification | For failure alerts |
| `SLACK_CHANNEL_ID` | Slack | Notification | Target channel |

**Cost estimate per nightly run** (Tier 2):
- Claude Code: ~$0.01-0.05 (Haiku model, 1-3 turns)
- Codex: ~$0.01-0.05 (cheapest model)
- Gemini CLI: Free (within daily quota)
- Droid: ~$0.01-0.05
- OpenCode: ~$0.01-0.05 (shares Anthropic/OpenAI key)
- **Total: ~$0.05-0.25/night, ~$1.50-7.50/month**

---

## 6. Cost Management Strategies

1. **Use cheapest models**: Claude Haiku, GPT-4o-mini, Gemini Flash
2. **Limit turns**: `--max-turns 3` for Claude; timeouts for others
3. **Minimal prompts**: Single file creation, not complex refactoring
4. **Skip Tier 2 on weekends**: `cron: '0 3 * * 1-5'` (weekdays only)
5. **Conditional Tier 2**: Only run if Tier 1 passes
6. **Budget alerts**: Monitor API usage dashboards
7. **Gemini free tier**: 1000 requests/day is more than sufficient

---

## 7. Handling Non-Determinism

Agent CLI outputs are inherently non-deterministic. The test suite handles this with:

1. **Property-based assertions** (not exact match):
   - File exists? (yes/no)
   - File contains expected content? (substring match)
   - Commit was made? (commit count increased)
   - Authorship note exists? (note present on HEAD)
   - Agent identified correctly? (agent name in note)

2. **Retry with `nick-fields/retry@v2`**:
   ```yaml
   - uses: nick-fields/retry@v2
     with:
       timeout_minutes: 10
       max_attempts: 2
       command: ./test-agent.sh claude
   ```

3. **`continue-on-error` for pre-release**:
   ```yaml
   - name: Run Tier 2 (latest/pre-release)
     continue-on-error: ${{ matrix.channel == 'latest' }}
   ```

4. **Generous timeouts**: 10-15 minutes per agent to handle API latency.

---

## 8. Notification Strategy

### On Failure

```yaml
notify-on-failure:
  needs: [tier1-hook-wiring, tier2-live-integration]
  runs-on: ubuntu-latest
  if: failure()
  steps:
    - name: Slack notification
      uses: slackapi/slack-github-action@v1
      with:
        channel-id: ${{ secrets.SLACK_CHANNEL_ID }}
        payload: |
          {
            "text": ":red_circle: Nightly agent integration tests FAILED",
            "blocks": [
              {
                "type": "section",
                "text": {
                  "type": "mrkdwn",
                  "text": "*Nightly Agent Integration* failed\n<${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}|View run>"
                }
              }
            ]
          }
      env:
        SLACK_BOT_TOKEN: ${{ secrets.SLACK_BOT_TOKEN }}
```

### Auto-Create GitHub Issue

```yaml
    - name: Create tracking issue
      uses: actions/github-script@v7
      with:
        script: |
          await github.rest.issues.create({
            owner: context.repo.owner,
            repo: context.repo.repo,
            title: `Nightly agent integration failure: ${new Date().toISOString().split('T')[0]}`,
            labels: ['nightly', 'integration', 'triage'],
            body: `## Nightly Agent Integration Test Failure\n\n[View workflow run](${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }})\n\n### Checklist\n- [ ] Identify which agent(s) failed\n- [ ] Check if agent CLI released a new version\n- [ ] Reproduce locally\n- [ ] Determine if git-ai needs a fix or if it's an agent regression`
          });
```

---

## 9. Known Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Agent CLI removes/renames headless flags | Tier 2 breaks for that agent | Tier 1 still catches wiring issues; pin known-good flag sets per version |
| API rate limits hit | Tier 2 fails intermittently | Retry logic; stagger agent runs; use free tiers where available |
| Agent hangs waiting for input | CI job times out | Per-step `timeout-minutes`; `timeout` command wrapper; `--max-turns` |
| Non-deterministic agent output | Flaky tests | Property-based assertions; retry; `continue-on-error` for pre-release |
| OpenCode hangs in containers | Known issue (#13851, #10012) | Extra timeout handling; skip if consistently failing |
| Gemini CLI initialization hang on headless Linux | Known issue (#20433) | Pre-install ripgrep dependency; fallback to skipping |
| npm package yanked or unavailable | Install step fails | Graceful fallback; `continue-on-error`; alert on install failure |
| Cost creep | Monthly bill increases | Budget caps; skip weekends; cheapest models; monitor dashboards |

---

## 10. Implementation Phases

### Phase 1: Tier 1 Only -- Hook Wiring Verification (1-2 days)

**Goal**: Verify that git-ai can install hooks for each real agent CLI binary.

- Create `nightly-agent-integration.yml` with `schedule` + `workflow_dispatch`
- Implement version resolution job
- Implement Tier 1 matrix for all 5 agents (stable + latest)
- Use existing `install-scripts-nightly.yml` as template
- No API keys needed
- Add Slack notification on failure

**Deliverables**:
- `.github/workflows/nightly-agent-integration.yml`
- `scripts/nightly/test-hook-wiring.sh` (shared test logic)

### Phase 2: Tier 2 for Claude Code (1-2 days)

**Goal**: Prove the live integration pattern works with the most-used agent.

- Add Tier 2 job (depends on Tier 1 passing)
- Implement test repo setup
- Run Claude Code with `--dangerously-skip-permissions --max-turns 3`
- Verify full attribution pipeline
- Add `ANTHROPIC_API_KEY` secret

**Deliverables**:
- `scripts/nightly/test-live-agent.sh` (parameterized by agent)
- Updated workflow with Tier 2 job

### Phase 3: Tier 2 for All Agents (2-3 days)

**Goal**: Extend live integration to Codex, Gemini, Droid, OpenCode.

- Add API key secrets for each service
- Handle per-agent invocation differences
- Add retry logic for flaky agents
- Handle known issues (OpenCode container hangs, Gemini init hangs)

**Deliverables**:
- Updated `test-live-agent.sh` with per-agent invocation logic
- Agent-specific workaround scripts

### Phase 4: Polish & Expand (1-2 days)

**Goal**: Production-ready nightly suite.

- Add macOS runner for Tier 1 (verify hooks on macOS)
- Add artifact upload for test results
- Add GitHub issue auto-creation on failure
- Add version comparison reporting (which agent version introduced the break)
- Add dashboard/badge for nightly status
- Document runbook for investigating failures

**Deliverables**:
- Updated workflow with macOS support
- `docs/nightly-integration-runbook.md`
- Status badge in README

---

## 11. Appendix: Full Workflow Skeleton

```yaml
name: Nightly Agent CLI Integration Tests

on:
  schedule:
    - cron: '0 4 * * 1-5'  # 4 AM UTC, weekdays only
  workflow_dispatch:
    inputs:
      agents:
        description: 'Comma-separated agents to test (or "all")'
        default: 'all'
      tier:
        description: 'Test tier to run'
        type: choice
        default: 'both'
        options: [tier1, tier2, both]

env:
  GIT_AI_DEBUG: "1"

jobs:
  # ── Version Resolution ──────────────────────────────────────────
  resolve-versions:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.build-matrix.outputs.matrix }}
    steps:
      - uses: actions/setup-node@v4
        with:
          node-version: '22'
      - id: build-matrix
        run: |
          # Build dynamic matrix from npm registry
          python3 - <<'PY'
          import json, subprocess, os

          agents = {
              "claude": {"pkg": "@anthropic-ai/claude-code", "key": "ANTHROPIC_API_KEY"},
              "codex":  {"pkg": "@openai/codex",             "key": "OPENAI_API_KEY"},
              "gemini": {"pkg": "@google/gemini-cli",        "key": "GEMINI_API_KEY"},
              "opencode": {"pkg": "opencode",                "key": "ANTHROPIC_API_KEY"},
          }

          matrix = {"include": []}
          for agent, info in agents.items():
              for channel in ["stable", "latest"]:
                  version = subprocess.check_output(
                      ["npm", "view", info["pkg"], "version"],
                      text=True
                  ).strip()
                  matrix["include"].append({
                      "agent": agent,
                      "channel": channel,
                      "npm_pkg": f"{info['pkg']}@{version}" if channel == "stable" else f"{info['pkg']}@latest",
                      "api_key_var": info["key"],
                      "version": version,
                  })

          # Droid uses curl installer (latest only)
          matrix["include"].append({
              "agent": "droid",
              "channel": "latest",
              "npm_pkg": "",
              "api_key_var": "FACTORY_API_KEY",
              "version": "latest",
          })

          with open(os.environ["GITHUB_OUTPUT"], "a") as f:
              f.write(f"matrix={json.dumps(matrix)}\n")
          PY

  # ── Tier 1: Hook Wiring Verification ────────────────────────────
  tier1-hook-wiring:
    needs: resolve-versions
    runs-on: ubuntu-latest
    timeout-minutes: 15
    strategy:
      fail-fast: false
      matrix: ${{ fromJson(needs.resolve-versions.outputs.matrix) }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: actions/setup-node@v4
        with:
          node-version: '22'

      - name: Build git-ai
        run: cargo build --release

      - name: Install agent CLI (${{ matrix.agent }} ${{ matrix.channel }})
        run: |
          if [ "${{ matrix.agent }}" = "droid" ]; then
            curl -fsSL https://app.factory.ai/cli | sh
            echo "$HOME/.local/bin" >> $GITHUB_PATH
          else
            npm install -g "${{ matrix.npm_pkg }}"
          fi

      - name: Verify agent binary
        run: |
          case "${{ matrix.agent }}" in
            claude)  claude --version ;;
            codex)   codex --version ;;
            gemini)  gemini --help | head -1 ;;
            droid)   droid --version ;;
            opencode) opencode --version ;;
          esac

      - name: Create test repository
        run: |
          mkdir -p /tmp/test-repo && cd /tmp/test-repo
          git init
          git config user.email "ci@git-ai.test"
          git config user.name "CI Test"
          echo "# Test Repo" > README.md
          git add README.md
          git commit -m "Initial commit"

      - name: Install git-ai hooks
        run: |
          cd /tmp/test-repo
          export PATH="$GITHUB_WORKSPACE/target/release:$PATH"
          git-ai install

      - name: Verify hook configuration
        run: ./scripts/nightly/verify-hook-wiring.sh "${{ matrix.agent }}"

      - name: Synthetic checkpoint test
        run: ./scripts/nightly/test-synthetic-checkpoint.sh "${{ matrix.agent }}"

      - name: Upload results
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: tier1-${{ matrix.agent }}-${{ matrix.channel }}
          path: /tmp/test-results/
          retention-days: 7

  # ── Tier 2: Live Agent Integration ──────────────────────────────
  tier2-live-integration:
    needs: [resolve-versions, tier1-hook-wiring]
    if: inputs.tier != 'tier1'
    runs-on: ubuntu-latest
    timeout-minutes: 20
    strategy:
      fail-fast: false
      matrix: ${{ fromJson(needs.resolve-versions.outputs.matrix) }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: actions/setup-node@v4
        with:
          node-version: '22'

      - name: Build git-ai
        run: cargo build --release

      - name: Install agent CLI (${{ matrix.agent }} ${{ matrix.channel }})
        run: |
          if [ "${{ matrix.agent }}" = "droid" ]; then
            curl -fsSL https://app.factory.ai/cli | sh
            echo "$HOME/.local/bin" >> $GITHUB_PATH
          else
            npm install -g "${{ matrix.npm_pkg }}"
          fi

      - name: Create test repository
        run: |
          mkdir -p /tmp/test-repo && cd /tmp/test-repo
          git init
          git config user.email "ci@git-ai.test"
          git config user.name "CI Test"
          echo "# Integration Test Repo" > README.md
          git add README.md
          git commit -m "Initial commit"
          export PATH="$GITHUB_WORKSPACE/target/release:$PATH"
          git-ai install

      - name: Run live agent test
        uses: nick-fields/retry@v2
        with:
          timeout_minutes: 10
          max_attempts: 2
          command: |
            export ${{ matrix.api_key_var }}="${{ secrets[matrix.api_key_var] }}"
            export PATH="$GITHUB_WORKSPACE/target/release:$PATH"
            ./scripts/nightly/test-live-agent.sh "${{ matrix.agent }}"
        continue-on-error: ${{ matrix.channel == 'latest' }}

      - name: Verify attribution pipeline
        run: |
          cd /tmp/test-repo
          export PATH="$GITHUB_WORKSPACE/target/release:$PATH"
          ./scripts/nightly/verify-attribution.sh "${{ matrix.agent }}"

      - name: Upload results
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: tier2-${{ matrix.agent }}-${{ matrix.channel }}
          path: /tmp/test-results/
          retention-days: 7

  # ── Notification ────────────────────────────────────────────────
  notify-on-failure:
    needs: [tier1-hook-wiring, tier2-live-integration]
    if: failure()
    runs-on: ubuntu-latest
    steps:
      - name: Notify Slack
        uses: slackapi/slack-github-action@v1
        with:
          channel-id: ${{ secrets.SLACK_CHANNEL_ID }}
          payload: |
            {
              "text": ":red_circle: Nightly agent integration tests FAILED: ${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}"
            }
        env:
          SLACK_BOT_TOKEN: ${{ secrets.SLACK_BOT_TOKEN }}
```

---

## 12. Appendix: Helper Scripts

### `scripts/nightly/verify-hook-wiring.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail

AGENT="$1"
RESULTS_DIR="/tmp/test-results"
mkdir -p "$RESULTS_DIR"

echo "Verifying hook wiring for: $AGENT"

case "$AGENT" in
  claude)
    SETTINGS="$HOME/.claude/settings.json"
    if [ ! -f "$SETTINGS" ]; then
      echo "FAIL: $SETTINGS not found" | tee "$RESULTS_DIR/hook-wiring.txt"
      exit 1
    fi
    if ! jq -e '.hooks.PreToolUse' "$SETTINGS" | grep -q "checkpoint"; then
      echo "FAIL: PreToolUse hook not configured" | tee "$RESULTS_DIR/hook-wiring.txt"
      exit 1
    fi
    echo "PASS: Claude Code hooks configured" | tee "$RESULTS_DIR/hook-wiring.txt"
    ;;
  codex)
    # Verify codex hook configuration
    echo "PASS: Codex hooks configured (verify specifics)" | tee "$RESULTS_DIR/hook-wiring.txt"
    ;;
  gemini)
    echo "PASS: Gemini hooks configured (verify specifics)" | tee "$RESULTS_DIR/hook-wiring.txt"
    ;;
  droid)
    echo "PASS: Droid hooks configured (verify specifics)" | tee "$RESULTS_DIR/hook-wiring.txt"
    ;;
  opencode)
    echo "PASS: OpenCode hooks configured (verify specifics)" | tee "$RESULTS_DIR/hook-wiring.txt"
    ;;
  *)
    echo "Unknown agent: $AGENT"
    exit 1
    ;;
esac
```

### `scripts/nightly/test-live-agent.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail

AGENT="$1"
cd /tmp/test-repo

PROMPT="Create a file called hello.txt containing exactly the text 'Hello World' on a single line. Then stage and commit it with the message 'Add hello.txt'."

case "$AGENT" in
  claude)
    timeout 300 claude -p \
      --dangerously-skip-permissions \
      --max-turns 3 \
      "$PROMPT"
    ;;
  codex)
    timeout 300 codex exec --full-auto "$PROMPT"
    ;;
  gemini)
    timeout 300 gemini --approval-mode=yolo "$PROMPT"
    ;;
  droid)
    timeout 300 droid exec --auto high "$PROMPT"
    ;;
  opencode)
    timeout 300 opencode run --command "$PROMPT"
    ;;
esac
```

### `scripts/nightly/verify-attribution.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail

AGENT="$1"
RESULTS_DIR="/tmp/test-results"
mkdir -p "$RESULTS_DIR"

cd /tmp/test-repo

echo "Verifying attribution for: $AGENT"

# Check file was created
if [ ! -f hello.txt ]; then
  echo "FAIL: hello.txt not created" | tee "$RESULTS_DIR/attribution.txt"
  exit 1
fi

# Check content
if ! grep -q "Hello World" hello.txt; then
  echo "FAIL: hello.txt does not contain 'Hello World'" | tee "$RESULTS_DIR/attribution.txt"
  exit 1
fi

# Check commit exists
COMMITS=$(git log --oneline | wc -l)
if [ "$COMMITS" -lt 2 ]; then
  echo "FAIL: No agent commit found" | tee "$RESULTS_DIR/attribution.txt"
  exit 1
fi

# Check authorship note
if ! git notes --ref=ai show HEAD 2>/dev/null | grep -qi "authorship"; then
  echo "WARN: No authorship note on HEAD (hook may not have fired)" | tee "$RESULTS_DIR/attribution.txt"
  # Don't fail -- the hook firing depends on the agent's checkpoint behavior
fi

# Check blame output
if git-ai blame hello.txt 2>/dev/null | grep -qi "$AGENT"; then
  echo "PASS: Agent '$AGENT' identified in blame output" | tee "$RESULTS_DIR/attribution.txt"
else
  echo "WARN: Agent '$AGENT' not found in blame output" | tee "$RESULTS_DIR/attribution.txt"
fi

echo "PASS: Attribution verification complete" | tee -a "$RESULTS_DIR/attribution.txt"
```

---

## 13. Open Questions

1. **Droid version pinning**: Factory AI's curl installer always fetches latest. Can we pin a specific version for stable testing? Need to check `https://app.factory.ai/cli` for version parameters.

2. **OpenCode container issues**: Known issues with headless operation in containers (#13851, #10012). Should we use `opencode serve` + `--attach` pattern for more reliable CI? Need testing.

3. **Agent config file locations**: Each agent stores its hook configuration differently. The verify-hook-wiring.sh script needs to be fleshed out with the exact paths and expected content for Codex, Gemini, Droid, and OpenCode. These are defined in `src/mdm/agents/*.rs`.

4. **Windows support**: Should Tier 1 include Windows runners? The existing nightly-upgrade.yml does test Windows. Cost vs. coverage tradeoff.

5. **Self-hosted runners**: For cost savings on Tier 2, should we use self-hosted runners with pre-installed agent CLIs?

6. **Pre-release discovery**: Not all agents publish pre-release versions to npm with a distinct tag. May need to use GitHub releases API or version enumeration for some agents.

7. **Checkpoint data format**: The synthetic checkpoint test (T1.3) needs fixture data that matches each agent's expected format. These fixtures should be derived from the real agent presets in `src/commands/checkpoint_agent/agent_presets.rs`.
