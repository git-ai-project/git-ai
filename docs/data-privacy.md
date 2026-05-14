# Data Privacy

## OSS Mode

If you install Git AI open source and don't login **no code, prompts, or agent usage data is ever sent to Git AI**. Git AI runs entirely on your machine and writes attribution data into your local git repository and prompts to a local SQLite.

The only data Git AI sends externally in open source mode is error and exception telemetry, which is enabled by default to help us improve the tool. You can disable or redirect it at any time by turning `telemetry_oss` to `off`. See [configuration options](https://usegitai.com/docs/cli/configuration#configuration-options) for details.

## Data

### Local-only data

- **Prompts** are stored locally on the developer's laptop. They are never shared with teammates or Git AI.

### Data written to your git repository

AI attribution data is written to git notes and is readable by anyone with repo access:

- Model, agent, and accepted-rate percentages
- Which lines are AI-generated
- Git profile (name, email) of the person who steered each prompt

### Telemetry

- **Error & exception telemetry** — shared by default with Git AI. You can disable it or redirect it to your own endpoint. See [configuration options](https://usegitai.com/docs/cli/configuration#configuration-options).

---

## Git AI Cloud (Personal Dashboards)

If you opt in to a personal dashboard, the following is uploaded to Git AI Cloud:

- **Agent usage telemetry** — cross-agent telemetry for every tool use, MCP call, skill invocation, interruption, error, and token-usage event, along with prompts and agent responses, used for your personal analytics
- **Personal Agent usage data** - % AI, # of Parallel agents and other stats on the dashboard are visible only to you unless you share them with others. 
- **SCM profile metadata** from GitHub, Bitbucket, or GitLab

See our [Cloud Privacy Policy](https://usegitai.com/privacy-policy) for details.

---

## Git AI for Teams and Enterprise

Teams and Enterprise deployments store additional data in the team instance:

- **Employee identity** — names, emails, and GitHub/Bitbucket/GitLab team membership
- **Agent usage telemetry** — cross-agent telemetry for every tool use, MCP call, skill invocation, interruption, error, and token-usage event
- **Full prompts** uploaded to the Git AI prompt store
  - Best-effort stripping of secrets and PII by default
  - You can apply your own filters for enhanced detection
  - Write-only: prompts are saved to your team's instance, but developers cannot read unless explicitly granted permission. 
- **Full agent sessions** can be reviewed, summarized, and made readable to developers
- **SCM PR data** from GitHub, Bitbucket, and GitLab
  - PR metadata: description, opener, reviewer, status
  - PR diffs — processed for computing % AI code, but not stored
- **Error & exception telemetry** — shared by default with Git AI, unless disabled or redirected to your own endpoint. See [configuration options](https://usegitai.com/docs/cli/configuration#configuration-options).

For more information, see our [Trust Center](https://trust.usegitai.com).

In [self-hosted deployments](https://github.com/git-ai-project/self-hosted), all data is sent only to your team's Git AI instance.
