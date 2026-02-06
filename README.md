<div>
<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/git-ai.png" align="right"
     alt="Git AI by git-ai-project/git-ai" width="100" height="100" />

</div>
<div>
<h1 align="left"><b>git-ai</b></h1>
</div>
<p align="left">Track the AI Code in your repositories</p>

<video src="https://github.com/user-attachments/assets/68304ca6-b262-4638-9fb6-0a26f55c7986" muted loop controls autoplay></video>

## Quick Start

#### Mac, Linux, Windows (WSL)

```bash
curl -sSL https://usegitai.com/install.sh | bash
```

#### Windows (non-WSL)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm http://usegitai.com/install.ps1 | iex"
```

🎊 That's it! **No per-repo setup.** Once installed Git AI will work OOTB with any of these **Supported Agents**:

<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/supported-agents.png" width="320" />

### Documentation https://usegitai.com/docs
- [AI Blame](https://usegitai.com/docs/cli/ai-blame)
- [Cross-Agent Prompt Saving](https://usegitai.com/docs/cli/prompt-storage)
- [CLI Reference](https://usegitai.com/docs/cli/reference)
- [Configuring Git AI for the enterprise](https://usegitai.com/docs/cli/configuration)

### Just Install and Commit

Build as usual. Just prompt, edit and commit. Git AI will track every line of AI-Code and record the Coding Agent, Model, and prompt that generated it. 

<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/graph.jpg" width="400" />

#### How Does it work? 

Supported Coding Agents call Git AI and mark the lines they insert as AI-generated. 

On commit, Git AI saves the final AI-attributions into a Git Note. These notes power AI-Blame, AI contribution stats, and more. The CLI makes sure these notes are preserved through rebases, merges, squashes, cherry-picks, etc.

![Git Tree](https://github.com/user-attachments/assets/edd20990-ec0b-4a53-afa4-89fa33de9541)

The format of the notes is outlined here in the [Git AI Standard v3.0.0](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md)

## Goals of `git-ai` project

🤖 **Track AI code in a Multi-Agent** world. Because developers get to choose their tools, engineering teams need a **vendor agnostic** way to track AI impact in their repos.

🎯 **Accurate attribution** from Laptop → Pull Request → Merged. Claude Code, Cursor and Copilot cannot track code after generation—Git AI follows it through the entire workflow.

🔄 **Support real-world git workflows** by making sure AI-Authorship annotations survive a `merge --squash`, `rebase`, `reset`, `cherry-pick` etc.

🔗 **Maintain link between prompts and code** - there is valuable context and requirements in team prompts—preserve them alongside code.

🚀 **Git-native + Fast** - `git-ai` is built on git plumbing commands. Negligible impact even in large repos (&lt;100ms). Tested in [Chromium](https://github.com/chromium/chromium).

## Agent Support

`git-ai` automatically sets up all supported agent hooks using the `git-ai install-hooks` command

| Agent/IDE                                                                                  | Authorship | Prompts |
| ------------------------------------------------------------------------------------------ | ---------- | ------- |
| Cursor &gt;1.7                                                                             | ✅         | ✅      |
| Claude Code                                                                                | ✅         | ✅      |
| GitHub Copilot in VSCode via Extension                                                     | ✅         | ✅      |
| OpenCode                                                                                   | ✅         | ✅      |
| Google Gemini CLI                                                                          | ✅         | ✅      |
| Continue CLI                                                                               | ✅         | ✅      |
| Droid CLI (Factory AI)                                                                     | ✅         | ✅      |
| Atlassian RovoDev CLI                                                                      | ✅         | ✅      |
| GitHub Copilot in Jetbrains IDEs (IntelliJ, etc.)                                          | ✅         | 🔄      |
| Jetbrains Junie                                                                            | ✅         | 🔄      |
| Amp (in-progress)                                                                          | 🔄         | 🔄      |
| AWS Kiro (in-progress)                                                                     | 🔄         | 🔄      |
| Continue VS Code/IntelliJ (in-progress)                                                    | 🔄         | 🔄      |
| Windsurf (in-review)                                                                       | 🔄         | 🔄      |
| Augment Code                                                                               | 🔄         | 🔄      |
| OpenAI Codex (waiting on [openai/codex #2109](https://github.com/openai/codex/issues/2109)) |            |         |
| Ona                                                                                        |            |         |
| Sourcegraph Cody                                                                           |            |         |
| Google Antigravity                                                                         |            |         |


> **Building a Coding Agent?** [Add support for Git AI by following this guide](https://usegitai.com/docs/cli/add-your-agent)

## Installing the Stats Bot (early access)

Aggregate `git-ai` data at the PR, developer, Repository and Organization levels:

- AI authorship breakdown for every Pull Request
- Measure % of code that is AI generated through the entire SDLC
- Compare accepted-rate for code written by each Agent + Model. 
- AI-Code Halflife (how durable is the AI code)
> [Get early access by chatting with the maintainers](https://calendly.com/acunniffe/meeting-with-git-ai-authors)

![alt](https://github.com/git-ai-project/git-ai/raw/main/assets/docs/dashboard.png)

