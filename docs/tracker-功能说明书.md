# Git-AI Tracker 功能说明书

**版本**: 1.3.1  
**更新日期**: 2026-04-17

---

## 1. 概述

Git-AI Tracker 是 easylife-ai 的代码追踪模块，在开发者执行 `git push` 时自动收集符合条件的 commit 信息并上报到远程服务器，用于团队代码贡献统计和 AI 辅助效率分析。

**设计原则**：
- 任何错误都不阻塞 `git push` 流程
- 上报失败自动进入重试队列
- 只上报用户真实编写的代码

---

## 2. 工作流程

```
git push
  │
  ├─ [pre-push] 通过 git ls-remote 获取远端当前 refs
  │
  ├─ 执行真实 git push
  │
  └─ [post-push] 对比 push 前后 refs 差异，找出新推送的 commits
         │
         └─ 对每个 commit 执行过滤检查
                │
                ├─ 通过 → 收集 diff + stats → 上报服务器
                │           ├─ 成功 → 打 refs/notes/ai-tracker 标记
                │           └─ 失败 → 写入 retry queue
                └─ 过滤 → 跳过（不上报）
```

---

## 3. 上报内容

每次上报的 payload 包含以下字段：

| 字段 | 来源 | 说明 |
|------|------|------|
| `team_id` | tracker-config.json | 团队 ID（整数） |
| `team_key` | tracker-config.json | 团队密钥 |
| `repo_url` | `git remote get-url <remote>` | 远端仓库 URL |
| `pushed_at` | 当前时间 UTC | Push 发生时间 |
| `pusher_email` | git config user.email（4 层兜底） | 推送者邮箱 |
| `pusher_name` | git config user.name（4 层兜底） | 推送者姓名 |
| `local_ref` | push 的分支名 | 本地分支（如 `main`） |
| `remote_ref` | push 的分支名 | 远端分支（如 `main`） |
| `commits[].commit_sha` | git | Commit SHA |
| `commits[].commit_author_email` | 同 pusher_email | 作者邮箱 |
| `commits[].commit_author_name` | 同 pusher_name | 作者姓名 |
| `commits[].commit_message` | `git log -1 --format=%s` | Commit message |
| `commits[].commit_timestamp` | `git log -1 --format=%cI`（UTC） | Committer 时间 |
| `commits[].git_ai_raw` | `easylife-ai stats --json` | AI 辅助统计数据 |
| `commits[].git_ai_version` | `easylife-ai --version` | easylife-ai 版本 |
| `commits[].diff_gz` | git show（gzip + base64） | 代码 diff |

### 3.1 Pusher 身份识别（4 层兜底）

按优先级依次尝试：

1. `git config --local user.email/name`（repo 级别配置）
2. `git config --global user.email/name`（全局配置）
3. `git log -1 --format=%ae/%an <commit_sha>`（commit 的 author 信息）
4. `hostname@localhost` / `hostname`（主机名兜底）

### 3.2 Diff 收集规则

- **支持的代码文件扩展名**：`.rs` `.py` `.js` `.ts` `.tsx` `.jsx` `.go` `.java` `.c` `.cpp` `.h` `.hpp` `.rb` `.php` `.swift` `.kt` `.scala` `.sh` `.sql` `.css` `.html` `.vue` `.svelte`
- **大小限制**：单个 commit 的 diff 截断至 100KB
- **压缩方式**：gzip 压缩后 base64 编码

---

## 4. 过滤规则

以下类型的 commit 会被过滤，不进行上报：

### 4.1 已上报过滤

- **检查**：`refs/notes/ai-tracker` note 是否存在
- **目的**：避免重复上报

### 4.2 Merge Commit 过滤

- **检查**：`git log -1 --format=%P` 父节点数量 > 1
- **目的**：Merge commit 不包含新代码

### 4.3 Synthetic Message 过滤

Commit message 匹配以下任一规则时过滤：

| 规则 | 示例 |
|------|------|
| 以 `merge ` 开头 | `Merge branch 'feature'` |
| 以 `merge branch` 开头 | `Merge branch main into dev` |
| 以 `merge pull request` 开头 | `Merge pull request #123` |
| 以 `revert ` 开头 | `Revert "feat: add feature"` |
| 以 `cherry-pick ` 开头 | `cherry-pick abc123` |
| 以 `rebase ` 开头 | `rebase onto main` |
| 包含 `cherry picked from commit` | `(cherry picked from commit abc123)` |

- **错误处理**：git 命令失败时保守处理，视为 synthetic（过滤）

### 4.4 Copy-Paste 阈值过滤

- **计算公式**：`manual_lines = git_diff_added_lines - ai_additions`
- **阈值**：`manual_lines > 1500`
- **数据来源**：`easylife-ai stats --json <commit_sha>`
- **错误处理**：stats 命令失败时 fail-open（不过滤，允许上报）

### 4.5 Blacklist 过滤

- **检查**：repo 的 remote URL 是否包含 blacklist 中的任意字符串（子串匹配）
- **匹配字段**：`git remote get-url <remote>` 返回的 URL（如 `http://192.168.1.1:9080/team/my-repo.git`）
- **配置**：`~/.git-ai/tracker-config.json` 的 `blacklist` 数组，推荐存储完整 remote URL

---

## 5. 失败重试机制

### 5.1 Retry Queue

上报失败时，commit 信息写入 `~/.git-ai/tracker-retry-queue.json`：

```json
[
  {
    "repo_path": "/Users/user/project/.git",
    "commit_sha": "abc123...",
    "diff_gz": [...],
    "retry_count": 0,
    "remote": "origin",
    "branch": "main"
  }
]
```

### 5.2 最大重试次数

- **限制**：每个 commit 最多重试 3 次（`MAX_RETRIES = 3`）
- **超限处理**：达到上限后从队列中丢弃

### 5.3 手动触发重试

```bash
easylife-ai tracker retry
```

处理 retry queue 中的所有条目，成功后从队列中移除并打上 `refs/notes/ai-tracker` 标记。

---

## 6. 配置文件

### 6.1 Tracker 配置

**路径**：`~/.git-ai/tracker-config.json`

```json
{
  "tracker_url": "http://your-tracker-server.com",
  "team_id": "1",
  "team_key": "your-team-key",
  "blacklist": ["test-repo", "playground"]
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `tracker_url` | string | 是 | Tracker 服务器地址 |
| `team_id` | string | 是 | 团队 ID |
| `team_key` | string | 是 | 团队密钥（HTTP Header `X-Team-Key`） |
| `blacklist` | array | 否 | 黑名单，子串匹配 repo 的 remote URL |

配置文件不存在时，tracker 静默跳过（不报错，不阻塞 push）。

#### 6.1.1 安装时自动配置

安装脚本支持通过环境变量自动创建 tracker 配置文件。

**本地脚本安装（推荐）**

用户本地已有 `install-easylife-ai.sh` 或 `install-easylife-ai.ps1` 脚本时：

Linux/macOS:
```bash
TRACKER_URL="http://your-server.com" \
TEAM_ID="1" \
TEAM_KEY="your-key" \
bash install-easylife-ai.sh
```

Windows PowerShell:
```powershell
$env:TRACKER_URL = "http://your-server.com"
$env:TEAM_ID = "1"
$env:TEAM_KEY = "your-key"
.\install-easylife-ai.ps1
```

**远程脚本安装**

从 GitHub 下载并安装：

Linux/macOS:
```bash
TRACKER_URL="http://your-server.com" \
TEAM_ID="1" \
TEAM_KEY="your-key" \
curl -sSL https://github.com/easylife88-2026/easylife-ai/releases/latest/download/install-easylife-ai.sh | bash
```

Windows PowerShell:
```powershell
$env:TRACKER_URL = "http://your-server.com"
$env:TEAM_ID = "1"
$env:TEAM_KEY = "your-key"
irm https://github.com/easylife88-2026/easylife-ai/releases/latest/download/install-easylife-ai.ps1 | iex
```

**行为说明**：
- 三个环境变量（`TRACKER_URL`、`TEAM_ID`、`TEAM_KEY`）必须同时提供，缺一不创建配置文件
- 如果 `tracker-config.json` 已存在，会覆盖 `tracker_url`、`team_id`、`team_key` 字段，但保留原有 `blacklist`
- 如果文件不存在，创建新文件，`blacklist` 默认为空数组 `[]`
- 配置写入失败不会中断安装流程，仅输出警告信息

### 6.2 Git-AI 主配置

**路径**：`~/.git-ai/config.json`

```json
{
  "feature_flags": {
    "async_mode": true
  }
}
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `feature_flags.async_mode` | bool | `true` | 是否异步上报（不阻塞 push） |

---

## 7. 命令行接口

### 7.1 处理 Retry Queue

```bash
easylife-ai tracker retry
```

- 读取 `~/.git-ai/tracker-retry-queue.json`
- 对每个条目重新上报
- 成功后清空队列
- 输出：`tracker retry queue processed`

### 7.2 查看上报日志

```bash
easylife-ai tracker log           # 查看最后 100 行
easylife-ai tracker log -n 50     # 查看最后 50 行
```

日志文件路径：`~/.git-ai/tracker-upload.log`

日志格式（每行）：

```
2026-04-17 14:32:45 ✓ 上报成功  e5067b4  ai-tracker/origin/main
2026-04-17 14:32:44 ⊘ 已跳过   c04b0ae  ai-tracker/origin/dev  - 已上报过
2026-04-17 14:33:22 ✗ 上报失败  8e7b16a  ai-tracker/origin/main - http://...: Connection refused
2026-04-17 14:33:25 ↻ 重试成功  8e7b16a  ai-tracker/origin/main
```

| 状态标识 | 含义 |
|---------|------|
| `✓ 上报成功` | commit 成功上报到服务器 |
| `⊘ 已跳过` | commit 被过滤规则跳过，后附跳过原因 |
| `✗ 上报失败` | 上报失败，已写入 retry queue，后附错误信息 |
| `↻ 重试成功` | retry queue 中的 commit 重试上报成功 |

跳过原因说明：

| 原因 | 含义 |
|------|------|
| `已上报过` | 该 commit 已有 `refs/notes/ai-tracker` 标记 |
| `黑名单过滤` | repo remote URL 匹配 blacklist |
| `合并提交` | merge commit，父节点数 > 1 |
| `自动生成的提交信息` | message 匹配 merge/revert/cherry-pick 等前缀 |
| `手动添加代码超过阈值（>300行）` | 非 AI 代码行数超过 300 行 |

### 7.3 黑名单管理

```bash
easylife-ai tracker blacklist list                    # 列出所有黑名单条目
easylife-ai tracker blacklist add                     # 将当前 repo 的 remote URL 加入黑名单
easylife-ai tracker blacklist add <repo_url>          # 手动指定 URL 加入黑名单
easylife-ai tracker blacklist remove                  # 将当前 repo 的 remote URL 从黑名单移除
easylife-ai tracker blacklist remove <repo_url>       # 手动指定 URL 从黑名单移除
```

- 无参数时自动读取当前目录 `git remote get-url origin` 作为 pattern
- 非 git 目录或无 remote origin 时报错提示
- blacklist 存储在 `~/.git-ai/tracker-config.json` 的 `blacklist` 数组中，为全局配置

---

## 8. API 接口

**URL**：`POST {tracker_url}/ai-code-boost/open/report/stats`

**Headers**：
```
Content-Type: application/json
X-Team-Key: {team_key}
```

**成功响应**：HTTP 2xx

---

## 9. 已知限制

### 9.1 Rebase/Cherry-pick 检测

同一用户 rebase 自己的 commit 无法可靠检测（git 保留原始 author timestamp，`refs/notes/ai` 会被 rebase hook 复制）。当前依赖 message 前缀检测，用户手动 rebase 自己的代码会被上报（代码确实是本人编写，可接受）。

### 9.2 Copy-Paste 阈值固定

阈值固定为 300 行，不支持配置。自动生成代码（protobuf、swagger）、大型配置文件（package-lock.json）可能被误过滤。

### 9.3 Diff 大小限制

单个 commit 的 diff 超过 100KB 时会被截断，服务端收到的是不完整的 diff。

---

## 10. 故障排查

| 现象 | 可能原因 | 解决方案 |
|------|---------|---------|
| push 后无 `uploaded` 输出 | 配置文件不存在 | 检查 `~/.git-ai/tracker-config.json` |
| push 后无 `uploaded` 输出 | commit 被过滤 | 执行 `easylife-ai tracker log` 查看跳过原因 |
| `✗ 上报失败` | 网络问题 | 检查 `tracker_url` 可达性；稍后执行 `easylife-ai tracker retry` |
| 重复上报 | note 丢失 | `git notes --ref=ai-tracker add -m "reported" <sha>` |
| 正常 commit 被过滤 | 行数超过阈值 | 分批提交 |
| 日志中大量 `已上报过` | 正常现象，push 时扫描了已上报的历史 commit | 无需处理 |
