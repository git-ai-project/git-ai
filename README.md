<div>
<img src="https://github.com/acunniffe/git-ai/raw/main/assets/docs/git-ai.png" align="right"
     alt="Git AI by acunniffe/git-ai" width="100" height="100" />

</div>
<div>
<h1 align="left"><b>git-ai</b></h1>
</div>
<p align="left">Track the AI Code in your repositories</p>

<video src="https://github.com/user-attachments/assets/68304ca6-b262-4638-9fb6-0a26f55c7986" muted loop controls autoplay></video>

## Quick Start

#### Mac, Linux, Windows (WSL)

```bash
curl -sSL https://raw.githubusercontent.com/acunniffe/git-ai/main/install.sh | bash
```

#### Windows (non-WSL)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/acunniffe/git-ai/main/install.ps1 | iex"
```

🎊 That's it! **No per-repo setup.** Once installed Git AI will work OOTB with any of these **Supported Agents**:

<img src="https://github.com/acunniffe/git-ai/raw/main/assets/docs/supported-agents.png" width="320" />

### Next step: **Just code and commit!**

Once installed, Cursor, Claude Code and GitHub Copilot will automatically call the CLI to mark the code they generate as AI-authored.

When you create a new commit, `git-ai` adds a git note that tracks which lines were AI-authored and prints the code authorship:

<img src="https://github.com/acunniffe/git-ai/raw/main/assets/docs/graph.jpg" width="400" />

If you're curious about the AI authorship of any file `git-ai blame` will show you which lines are AI generated:

![alt](https://github.com/acunniffe/git-ai/raw/main/assets/docs/blame-cmd.jpg)

## Installing the PR Bot (currently in-development)

A PR Bot aggregates `git-ai` data at the PR, Repository and Organization levels:

- AI authorship breakdown for every Pull Request
- Measure % of code that is AI generated (and the trend)
- AI Cost per-PR and accepted AI rate (measures how often AI code is accepted on first generation)

> [Get early access by chatting with the maintainers](https://calendly.com/acunniffe/meeting-with-git-ai-authors)

![alt](https://github.com/acunniffe/git-ai/raw/main/assets/docs/bot.jpg)

## Goals of `git-ai` project

🤖 **Track AI code in a Multi-Agent** world. Because developers get to choose their tools, engineering teams need a **vendor agnostic** way to track AI impact in their repos.

🎯 **Accurate attribution** from Laptop → Pull Request → Merged. Claude Code, Cursor and Copilot cannot track code after generation—Git AI follows it through the entire workflow.

🔄 **Support real-world git workflows** by making sure AI-Authorship annotations survive a `merge --squash`, `rebase`, `reset`, `cherry-pick` etc.

🔗 **Maintain link between prompts and code** - there is valuable context and requirements in team prompts—preserve them alongside code.

🚀 **Git-native + Fast** - `git-ai` is built on git plumbing commands. Negligible impact even in large repos (&lt;100ms). Tested in [Chromium](https://github.com/chromium/chromium).

## Documentation

- How Git AI Works and its Limitations [▶️ Video](https://www.youtube.com/watch?v=b_DZTC1PKHI) [🗺️ Diagram](https://usegitai.com/docs/how-git-ai-works)
- [Git AI Commands](https://usegitai.com/docs/reference)
- [Configuring Git AI for the enterprise](https://usegitai.com/docs/enterprise-configuration)

## Agent Support

`git-ai` automatically sets up all supported agent hooks using the `git-ai install-hooks` command

| Agent/IDE                                                                                  | Authorship | Prompts |
| ------------------------------------------------------------------------------------------ | ---------- | ------- |
| [Cursor &gt;1.7](https://usegitai.com/docs/cursor)                                         | ✅         | ✅      |
| [Claude Code](https://usegitai.com/docs/claude-code)                                       | ✅         | ✅      |
| [GitHub Copilot in VSCode via Extension](https://usegitai.com/docs/vs-code-github-copilot) | ✅         | ✅      |
| OpenAI Codex (waiting on [openai/codex #2109](https://github.com/openai/codex/issues/2109)) |            |         |
| Sourcegraph Cody + Amp                                                                     |            |         |
| Windsurf                                                                                   |            |         |
| RovoDev CLI                                                                                |            |         |
| _your agent here_                                                                          |            |         |

> **Building a Coding Agent?** [Add support for Git AI by following this guide](https://usegitai.com/docs/add-your-agent)
# Test rebase approach
