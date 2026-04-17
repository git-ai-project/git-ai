# Git-AI Tracker 测试用例文档

**版本**: 1.3.1  
**更新日期**: 2026-04-17

---

## 测试环境

- **二进制**：`~/.git-ai/bin/easylife-ai`
- **配置**：`~/.git-ai/tracker-config.json`
- **测试仓库**：任意 git 仓库，已配置 remote
- **验证方式（上报状态）**：`git notes --ref=ai-tracker show <sha>`，返回 `reported` 为已上报，`error: no note found` 为未上报
- **验证方式（日志）**：`easylife-ai tracker log -n 5` 查看最近上报记录

---

## 第一组：正常上报（应上报）

### TC-01 普通单文件修改

**前置条件**：tracker-config.json 存在，服务器可达

**步骤**：
```bash
echo "hello" > test.txt
git add test.txt
git commit -m "feat: add test file"
SHA=$(git rev-parse HEAD)
git push origin <branch>
```

**预期结果**：
- 输出 `[git-ai tracker] uploaded <sha>`
- `git notes --ref=ai-tracker show $SHA` 返回 `reported`

---

### TC-02 多文件修改

**步骤**：
```bash
echo "a" > a.txt && echo "b" > b.txt && echo "c" > c.txt
git add . && git commit -m "feat: add multiple files"
git push origin <branch>
```

**预期结果**：commit 被上报，note = `reported`

---

### TC-03 一次 push 多个 commit

**步骤**：
```bash
echo "1" > c1.txt && git add . && git commit -m "feat: commit 1"
echo "2" > c2.txt && git add . && git commit -m "feat: commit 2"
echo "3" > c3.txt && git add . && git commit -m "feat: commit 3"
git push origin <branch>
```

**预期结果**：
- 输出 3 行 `[git-ai tracker] uploaded <sha>`
- 3 个 commit 均有 `reported` note

---

### TC-04 新增文件

**步骤**：
```bash
echo "new file content" > new-feature.py
git add new-feature.py && git commit -m "feat: add new python file"
git push origin <branch>
```

**预期结果**：commit 被上报

---

### TC-05 删除文件

**步骤**：
```bash
git rm some-file.txt && git commit -m "chore: remove unused file"
git push origin <branch>
```

**预期结果**：commit 被上报（删除操作也是用户行为）

---

### TC-06 小量代码新增（< 300 行）

**步骤**：
```bash
python3 -c "print('\n'.join(['line '.format(i) for i in range(100)]))" > small.py
git add small.py && git commit -m "feat: add small file"
git push origin <branch>
```

**预期结果**：commit 被上报（100 行 < 300 行阈值）

---

## 第二组：过滤规则（不应上报）

### TC-11 Merge Commit（--no-ff）

**步骤**：
```bash
git checkout -b feature-branch
echo "feature" > feature.txt && git add . && git commit -m "feat: feature"
git checkout main
git merge --no-ff feature-branch -m "Merge branch 'feature-branch'"
MERGE_SHA=$(git rev-parse HEAD)
git push origin main
```

**预期结果**：
- feature commit 被上报
- merge commit `$MERGE_SHA` 无 note（`error: no note found`）

---

### TC-12 Revert Commit

**步骤**：
```bash
echo "to revert" > revert-test.txt && git add . && git commit -m "feat: will be reverted"
ORIG_SHA=$(git rev-parse HEAD)
git revert HEAD --no-edit
REVERT_SHA=$(git rev-parse HEAD)
git push origin <branch>
```

**预期结果**：
- 原始 commit `$ORIG_SHA` 被上报
- revert commit `$REVERT_SHA` 无 note

---

### TC-13 Copy-Paste 大文件（> 300 行）

**步骤**：
```bash
python3 -c "print('\n'.join(['function func{}() {{ return {}; }}'.format(i,i) for i in range(350)]))" > large.js
git add large.js && git commit -m "feat: add large copied code"
SHA=$(git rev-parse HEAD)
git push origin <branch>
```

**预期结果**：commit 无 note（350 行 > 300 行阈值）

---

### TC-14 Merge Pull Request Message

**步骤**：
```bash
git commit --allow-empty -m "Merge pull request #42 from user/feature"
SHA=$(git rev-parse HEAD)
git push origin <branch>
```

**预期结果**：commit 无 note（message 以 `merge pull request` 开头）

---

### TC-15 Cherry-pick with -x

**步骤**：
```bash
# 在另一个分支创建 commit
git checkout -b source-branch
echo "source" > source.txt && git add . && git commit -m "feat: source commit"
SOURCE_SHA=$(git rev-parse HEAD)
git checkout main
git cherry-pick -x $SOURCE_SHA
PICKED_SHA=$(git rev-parse HEAD)
git push origin main
```

**预期结果**：cherry-pick commit 无 note（message 包含 `cherry picked from commit`）

---

### TC-16 Blacklist 过滤

**前置条件**：`tracker-config.json` 的 `blacklist` 包含当前 repo 的 remote URL（子串即可）

**步骤**：
```bash
# 方式一：自动加入当前 repo（推荐）
cd /path/to/my-repo
easylife-ai tracker blacklist add
# 输出：已将 'http://your-server/team/my-repo.git' 加入黑名单

# 方式二：手动指定
easylife-ai tracker blacklist add "my-repo"

echo "test" > test.txt && git add . && git commit -m "feat: blacklisted repo"
SHA=$(git rev-parse HEAD)
git push origin <branch>
```

**预期结果**：
- commit 无 note（`error: no note found`）
- retry queue 为空
- `easylife-ai tracker log -n 1` 显示 `⊘ 已跳过 ... 黑名单过滤`

**清理**：
```bash
easylife-ai tracker blacklist remove
```

---

### TC-17 已上报 Commit 不重复上报

**步骤**：
```bash
# 先正常 push 一次（commit 已上报）
git push origin <branch>
# 再次 push（无新 commit）
git push origin <branch>
```

**预期结果**：第二次 push 输出 `Everything up-to-date`，无 `uploaded` 输出

---

## 第三组：Retry Queue

### TC-21 服务器不可达时写入 Retry Queue

**步骤**：
```bash
# 设置无效 tracker_url
cat > ~/.git-ai/tracker-config.json << 'EOF'
{"tracker_url":"http://127.0.0.1:19999","team_id":"1","team_key":"test","blacklist":[]}
EOF
rm -f ~/.git-ai/tracker-retry-queue.json

echo "retry-test" > retry.txt && git add . && git commit -m "test: retry queue"
SHA=$(git rev-parse HEAD)
git push origin <branch>
```

**预期结果**：
- retry queue 文件存在，包含 `commit_sha`、`repo_path`、`diff_gz`、`retry_count: 0`
- commit 无 note

**清理**：恢复 tracker-config.json

---

### TC-22 手动重试成功

**前置条件**：TC-21 执行后，retry queue 中有条目

**步骤**：
```bash
# 恢复正确配置
cp ~/.git-ai/tracker-config.json.bak ~/.git-ai/tracker-config.json
# 手动重试
easylife-ai tracker retry
```

**预期结果**：
- 输出 `[git-ai tracker] uploaded <sha>`
- 输出 `tracker retry queue processed`
- retry queue 文件被删除
- commit 有 `reported` note
- `easylife-ai tracker log -n 1` 显示 `↻ 重试成功`

---

### TC-23 Retry 达到最大次数后丢弃

**步骤**：
```bash
# 注入 retry_count=2 的条目（一次失败后达到 MAX_RETRIES=3）
cat > ~/.git-ai/tracker-retry-queue.json << 'EOF'
[{"repo_path":"/path/to/repo/.git","commit_sha":"deadbeef...","diff_gz":[],"retry_count":2,"remote":"origin","branch":"main"}]
EOF

# 设置无效 URL 确保失败
cat > ~/.git-ai/tracker-config.json << 'EOF'
{"tracker_url":"http://127.0.0.1:19999","team_id":"1","team_key":"test","blacklist":[]}
EOF

easylife-ai tracker retry
```

**预期结果**：
- 输出 `tracker retry queue processed`
- retry queue 文件被删除（条目被丢弃）

---

## 第四组：Push 场景

### TC-31 删除远端分支（不触发上报）

**步骤**：
```bash
git push origin --delete some-branch
```

**预期结果**：无 `uploaded` 输出，无 retry queue 变化

---

### TC-32 Push Tag（不触发上报）

**步骤**：
```bash
git tag v1.0.0 HEAD
git push origin v1.0.0
```

**预期结果**：无 `uploaded` 输出

---

### TC-33 Force Push

**步骤**：
```bash
echo "force" > force.txt && git add . && git commit -m "feat: force push test"
SHA=$(git rev-parse HEAD)
git push --force origin <branch>
```

**预期结果**：commit 被正常上报，note = `reported`

---

### TC-34 Push 到新分支

**步骤**：
```bash
git checkout -b new-branch
echo "new" > new.txt && git add . && git commit -m "feat: new branch commit"
SHA=$(git rev-parse HEAD)
git push origin new-branch
```

**预期结果**：commit 被上报（新分支首次 push，远端无该分支，视为全新提交）

---

## 第五组：错误处理

### TC-41 配置文件不存在（不阻塞 push）

**步骤**：
```bash
mv ~/.git-ai/tracker-config.json ~/.git-ai/tracker-config.json.bak
echo "test" > test.txt && git add . && git commit -m "test: no config"
git push origin <branch>
```

**预期结果**：
- push 正常完成（exit code 0）
- 无 `uploaded` 输出
- 无报错

**清理**：恢复配置文件

---

### TC-42 easylife-ai stats 命令失败（不阻塞上报）

**步骤**：
```bash
# 临时重命名 easylife-ai 使 stats 命令失败
mv ~/.git-ai/bin/easylife-ai ~/.git-ai/bin/easylife-ai.bak
echo "test" > test.txt && git add . && git commit -m "test: stats fail"
SHA=$(git rev-parse HEAD)
git push origin <branch>
```

**预期结果**：
- commit 仍然被上报（stats 失败时 fail-open，不过滤）
- note = `reported`

**清理**：恢复 easylife-ai

---

### TC-43 网络超时（写入 Retry Queue，不阻塞 push）

**步骤**：
```bash
# 设置超时地址
cat > ~/.git-ai/tracker-config.json << 'EOF'
{"tracker_url":"http://10.255.255.1","team_id":"1","team_key":"test","blacklist":[]}
EOF

echo "timeout" > timeout.txt && git add . && git commit -m "test: network timeout"
SHA=$(git rev-parse HEAD)
time git push origin <branch>
```

**预期结果**：
- push 正常完成（不因网络超时阻塞）
- commit 进入 retry queue
- 总耗时不超过正常 push 时间 + 5 秒

**清理**：恢复配置文件

---

## 第六组：身份识别

### TC-51 有 git config user.email（正常路径）

**步骤**：
```bash
git config user.email "user@example.com"
git config user.name "Test User"
echo "test" > test.txt && git add . && git commit -m "test: identity"
git push origin <branch>
```

**预期结果**：上报的 `pusher_email` = `user@example.com`，`pusher_name` = `Test User`

---

### TC-52 无 local config，有 global config（兜底路径 2）

**步骤**：
```bash
git config --unset user.email
git config --unset user.name
git config --global user.email "global@example.com"
git config --global user.name "Global User"
echo "test" > test.txt && git add . && git commit -m "test: global identity"
git push origin <branch>
```

**预期结果**：上报的 `pusher_email` = `global@example.com`

---

### TC-53 无 git config，使用 commit author（兜底路径 3）

**步骤**：
```bash
git config --unset user.email
git config --unset user.name
git config --global --unset user.email
git config --global --unset user.name
git commit --allow-empty --author="Author User <author@example.com>" -m "test: author fallback"
git push origin <branch>
```

**预期结果**：上报的 `pusher_email` = `author@example.com`

---

---

## 第七组：上报日志（tracker log）

### TC-61 查看日志（默认 100 行）

**步骤**：
```bash
easylife-ai tracker log
```

**预期结果**：显示最后 100 行，每行格式为：
```
2026-04-17 14:32:45 ✓ 上报成功  e5067b4  ai-tracker/origin/main
```

---

### TC-62 查看日志（指定行数）

**步骤**：
```bash
easylife-ai tracker log -n 3
```

**预期结果**：仅显示最后 3 行

---

### TC-63 无日志文件时提示

**步骤**：
```bash
rm -f ~/.git-ai/tracker-upload.log
easylife-ai tracker log
```

**预期结果**：输出 `暂无上报日志`

---

## 第八组：黑名单管理（tracker blacklist）

### TC-64 列出黑名单（空）

**步骤**：
```bash
easylife-ai tracker blacklist list
```

**预期结果**：输出 `Blacklist is empty`

---

### TC-65 无参数 add（自动用 remote URL）

**前置条件**：在 git 仓库目录下，已配置 `origin` remote

**步骤**：
```bash
cd /path/to/my-repo
easylife-ai tracker blacklist add
```

**预期结果**：
- 输出 `已将 'http://your-server/team/my-repo.git' 加入黑名单`
- `easylife-ai tracker blacklist list` 显示该 URL

---

### TC-66 add 重复条目（应报错）

**步骤**：
```bash
easylife-ai tracker blacklist add "my-repo"
easylife-ai tracker blacklist add "my-repo"
```

**预期结果**：第二次输出 `Error: Pattern 'my-repo' already in blacklist`

---

### TC-67 无参数 remove（自动用 remote URL）

**前置条件**：当前 repo 的 remote URL 已在黑名单中

**步骤**：
```bash
cd /path/to/my-repo
easylife-ai tracker blacklist remove
```

**预期结果**：输出 `已将 'http://your-server/team/my-repo.git' 从黑名单移除`

---

### TC-68 remove 不存在的条目（应报错）

**步骤**：
```bash
easylife-ai tracker blacklist remove "nonexistent"
```

**预期结果**：输出 `Error: Pattern 'nonexistent' not found in blacklist`

---

### TC-69 非 git 目录下无参数 add（应报错）

**步骤**：
```bash
cd /tmp
easylife-ai tracker blacklist add
```

**预期结果**：
```
未检测到 git remote origin，请手动指定 repo URL
Usage: easylife-ai tracker blacklist add <repo_url>
```

---

## 测试结果汇总表

| 编号 | 测试用例 | 预期行为 | 实际结果 | 状态 |
|------|---------|---------|---------|------|
| TC-01 | 普通单文件修改 | 上报 | reported | ✅ |
| TC-02 | 多文件修改 | 上报 | reported | ✅ |
| TC-03 | 一次 push 多个 commit | 全部上报 | 3 个 reported | ✅ |
| TC-04 | 新增文件 | 上报 | reported | ✅ |
| TC-05 | 删除文件 | 上报 | reported | ✅ |
| TC-06 | 小量代码（< 300 行） | 上报 | reported | ✅ |
| TC-11 | Merge commit | 过滤 | no note，日志显示「合并提交」 | ✅ |
| TC-12 | Revert commit | 过滤 | no note，日志显示「自动生成的提交信息」 | ✅ |
| TC-13 | Copy-paste > 300 行 | 过滤 | no note | ✅ |
| TC-14 | Merge PR message | 过滤 | no note | ✅ |
| TC-15 | Cherry-pick -x | 过滤 | no note | ✅ |
| TC-16 | Blacklist 过滤（remote URL 匹配） | 过滤 | no note，日志显示「黑名单过滤」 | ✅ |
| TC-17 | 重复 push | 跳过 | 日志显示「已上报过」 | ✅ |
| TC-21 | 服务器不可达 → retry queue | 写入队列 | queue 有条目，日志显示「上报失败」 | ✅ |
| TC-22 | 手动重试成功 | 上报并清空队列 | reported + queue empty，日志显示「重试成功」 | ✅ |
| TC-23 | 达到最大重试次数 | 丢弃条目 | queue deleted | ✅ |
| TC-31 | 删除远端分支 | 不触发 | no upload，日志无新增 | ✅ |
| TC-32 | Push tag | 不触发 | no upload，日志无新增 | ✅ |
| TC-33 | Force push | 上报 | reported | ✅ |
| TC-34 | Push 到新分支 | 上报 | reported | ✅ |
| TC-41 | 配置文件不存在 | 不阻塞 push | push 成功 | ✅ |
| TC-42 | stats 命令失败 | fail-open，仍上报 | reported | 待验证 |
| TC-43 | 网络超时 | 不阻塞 push | push 成功 | 待验证 |
| TC-51 | 有 local git config | 使用 local config | 正确 email | ✅ |
| TC-52 | 无 local，有 global | 使用 global config | 正确 email | 待验证 |
| TC-53 | 无 git config | 使用 commit author | 正确 email | 待验证 |
| TC-61 | tracker log（默认 100 行） | 显示最后 100 行 | 正确 | ✅ |
| TC-62 | tracker log -n 3 | 显示最后 3 行 | 正确 | ✅ |
| TC-63 | tracker log（无日志文件） | 提示「暂无上报日志」 | 正确 | ✅ |
| TC-64 | blacklist list（空） | 显示「Blacklist is empty」 | 正确 | ✅ |
| TC-65 | blacklist add（无参数） | 自动用 remote URL | 正确 | ✅ |
| TC-66 | blacklist add（重复） | 报错 | 正确 | ✅ |
| TC-67 | blacklist remove（无参数） | 自动用 remote URL | 正确 | ✅ |
| TC-68 | blacklist remove（不存在） | 报错 | 正确 | ✅ |
| TC-69 | blacklist add（非 git 目录） | 报错提示 | 正确 | ✅ |
