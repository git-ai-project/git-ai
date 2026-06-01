# Git-AI S3 发布流程

本文档说明当前推荐的内部发布方式：

```text
GitHub Actions 构建所有平台 release artifact
-> 本地下载 artifact
-> 本地使用 s3cmd 上传到公司内网 S3
-> 用户通过固定频道 URL 安装
```

不再推荐在本机手动 Docker 构建各个平台。构建所有官方平台应交给 GitHub Actions matrix，上传内网 S3 则放在本机执行，因为 GitHub 官方 runner 通常无法访问公司内网 S3。

## 目标

当前内部 S3 release 平台包括 Linux 和 macOS：

```text
git-ai-linux-x64
git-ai-linux-arm64
git-ai-macos-x64
git-ai-macos-arm64
```

最终上传到公司 S3 的结构：

```text
s3://ep-zadig-prod/frontend/packages/tools/git-ai/
  v1.4.9-rebasefix-20260601/
    git-ai-linux-x64
    git-ai-linux-arm64
    git-ai-macos-x64
    git-ai-macos-arm64
    SHA256SUMS
    install.sh
    install.ps1

  channels/
    rebasefix/
      git-ai-linux-x64
      git-ai-linux-arm64
      git-ai-macos-x64
      git-ai-macos-arm64
      SHA256SUMS
      install.sh
      install.ps1
```

用户固定安装命令：

```bash
curl -fsSL https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/install.sh | bash
```

以后发布新版本，只需要更新频道目录，用户安装命令不变。

## 前置条件

本地需要：

```bash
gh
uv
```

确认 GitHub CLI 已登录：

```bash
gh auth status
```

确认公司 S3 配置存在：

```bash
ls -l ~/.s3cfg-prod
```

`~/.s3cfg-prod` 包含 access key 和 secret key，只能保存在本机或公司密钥系统中，不要提交到 Git。

确认 S3 可访问：

```bash
uv tool run s3cmd -c ~/.s3cfg-prod ls s3://ep-zadig-prod/frontend/packages/tools/git-ai/
```

## 1. 准备 GitHub Actions 构建分支

S3 发布 workflow 文件是：

```text
.github/workflows/release-s3.yml
```

GitHub 有一个限制：新的 `workflow_dispatch` 文件必须先存在于 fork 的默认分支，才能手动触发。因此推荐在 fork 内部合并一次发布流程 PR：

```text
congziqi77/git-ai:codex/s3-release-packaging -> congziqi77/git-ai:main
```

注意，这只影响 fork：

```text
congziqi77/git-ai
```

不会影响官方源仓库：

```text
git-ai-project/git-ai
```

也不要把 PR 开到官方源仓库。

## 2. 触发 GitHub Actions dry run 构建

在 fork 仓库执行：

```bash
gh workflow run "S3 Release Build" \
  --repo congziqi77/git-ai \
  --ref codex/s3-release-packaging \
  -f release_tag=v1.4.9-rebasefix-20260601 \
  -f s3_bucket=ep-zadig-prod \
  -f s3_prefix=frontend/packages/tools/git-ai \
  -f public_base_url=https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai \
  -f aws_region=us-east-1 \
  -f dry_run=true
```

`dry_run=true` 表示只构建 artifact，不上传 S3。

查看最新 run：

```bash
gh run list \
  --repo congziqi77/git-ai \
  --workflow "S3 Release Build" \
  --limit 5
```

查看某个 run 的状态：

```bash
gh run view <run-id> --repo congziqi77/git-ai
```

等待所有 build job 完成。成功后会有 Linux/macOS artifact，以及最终聚合 artifact：

```text
git-ai-linux-x64
git-ai-linux-arm64
git-ai-macos-x64
git-ai-macos-arm64
git-ai-s3-release-v1.4.9-rebasefix-20260601
```

## 3. 下载 GitHub Actions artifact

推荐下载最终聚合 artifact：

```bash
cd /Users/congziqi/Documents/goWork/git-ai
rm -rf build/github-artifacts build/git-ai-all
mkdir -p build/github-artifacts build/git-ai-all

gh run download <run-id> \
  --repo congziqi77/git-ai \
  --name git-ai-s3-release-v1.4.9-rebasefix-20260601 \
  --dir build/github-artifacts
```

如果最终聚合 artifact 还没有生成，也可以下载所有 artifact：

```bash
gh run download <run-id> \
  --repo congziqi77/git-ai \
  --dir build/github-artifacts
```

然后汇总二进制到 `build/git-ai-all`：

```bash
find build/github-artifacts -type f -name 'git-ai-*' -maxdepth 3 -exec cp {} build/git-ai-all/ \;
```

确认文件齐全：

```bash
ls -lh build/git-ai-all
```

必须看到：

```text
git-ai-linux-x64
git-ai-linux-arm64
git-ai-macos-x64
git-ai-macos-arm64
```

## 4. 本地 dry run 打包

下载齐 Linux/macOS 4 个平台 binary 后，先本地 dry run：

```bash
RELEASE_TAG=v1.4.9-rebasefix-20260601 \
S3_BUCKET=ep-zadig-prod \
S3_PREFIX=frontend/packages/tools/git-ai \
PUBLIC_BASE_URL=https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai \
UPLOAD_TOOL=s3cmd \
S3CMD_CONFIG=~/.s3cfg-prod \
CHANNEL=rebasefix \
PACKAGE_DIR=build/git-ai-all \
REQUIRE_ALL_PLATFORMS=1 \
DRY_RUN=1 \
scripts/release-s3-local.sh
```

这一步会生成：

```text
release/s3-v1.4.9-rebasefix-20260601/
release/channel-rebasefix/
```

并生成：

```text
SHA256SUMS
install.sh
install.ps1
```

检查生成的安装脚本：

```bash
rg 'PINNED_VERSION|BASE_URL|EMBEDDED_CHECKSUMS' release/s3-v1.4.9-rebasefix-20260601/install.sh
rg 'PINNED_VERSION|BASE_URL|EMBEDDED_CHECKSUMS' release/channel-rebasefix/install.sh
```

## 5. 上传到公司 S3

确认 dry run 没问题后，执行正式上传：

```bash
RELEASE_TAG=v1.4.9-rebasefix-20260601 \
S3_BUCKET=ep-zadig-prod \
S3_PREFIX=frontend/packages/tools/git-ai \
PUBLIC_BASE_URL=https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai \
UPLOAD_TOOL=s3cmd \
S3CMD_CONFIG=~/.s3cfg-prod \
CHANNEL=rebasefix \
PACKAGE_DIR=build/git-ai-all \
REQUIRE_ALL_PLATFORMS=1 \
scripts/release-s3-local.sh
```

脚本会上传两份：

```text
s3://ep-zadig-prod/frontend/packages/tools/git-ai/v1.4.9-rebasefix-20260601/
s3://ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/
```

确认 S3 文件：

```bash
uv tool run s3cmd -c ~/.s3cfg-prod ls s3://ep-zadig-prod/frontend/packages/tools/git-ai/v1.4.9-rebasefix-20260601/
uv tool run s3cmd -c ~/.s3cfg-prod ls s3://ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/
```

## 6. 下载和安装验证

先验证安装脚本可访问：

```bash
curl -fsSL https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/install.sh | head
```

测试机器安装：

```bash
curl -fsSL https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/install.sh | bash
```

安装后验证：

```bash
git-ai --version
which git-ai
which git
ls -l ~/.git-ai/bin/git ~/.git-ai/bin/git-ai ~/.git-ai/bin/git-og
```

预期：

```text
~/.git-ai/bin/git -> ~/.git-ai/bin/git-ai
~/.git-ai/bin/git-og -> 系统原始 git
```

如果当前 shell 没有加载新的 PATH，可以执行：

```bash
source ~/.zshrc
```

或者打开一个新的终端。

## 常用参数

`RELEASE_TAG`：内部发布版本号，例如：

```text
v1.4.9-rebasefix-20260601
```

`S3_BUCKET`：公司 bucket：

```text
ep-zadig-prod
```

`S3_PREFIX`：公司路径：

```text
frontend/packages/tools/git-ai
```

`PUBLIC_BASE_URL`：用户下载 URL 前缀：

```text
https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai
```

`CHANNEL`：可变频道名，例如：

```text
rebasefix
```

`PACKAGE_DIR`：从 GitHub artifact 汇总出来的二进制目录：

```text
build/git-ai-all
```

`REQUIRE_ALL_PLATFORMS=1`：强制要求 Linux/macOS 4 个平台 binary 都存在。正式发布建议始终开启。

## 故障排查

如果 `gh workflow run` 提示找不到 workflow，说明 `.github/workflows/release-s3.yml` 还没有进入 fork 默认分支。先把 S3 发布流程 PR 合并到 fork 的 `main`。

如果某些 GitHub Actions job 长时间 queued，通常是对应 runner 不可用或排队中，尤其是：

```text
ubuntu-22.04-arm
macos-15-intel
```

如果下载后 `REQUIRE_ALL_PLATFORMS=1` 报缺文件，说明对应平台 artifact 没有成功生成或没有复制到 `build/git-ai-all`。

如果上传失败，先确认 S3 配置：

```bash
uv tool run s3cmd -c ~/.s3cfg-prod ls s3://ep-zadig-prod/frontend/packages/tools/git-ai/
```

如果安装时 checksum 失败，说明 S3 上的二进制和 `install.sh` 内嵌 checksum 不匹配。重新下载 artifact，重新执行本地 dry run 和正式上传。

## 安全提醒

不要把 `~/.s3cfg-prod` 内容提交到 Git，也不要贴到 issue、PR、日志或聊天里。该文件包含明文 access key 和 secret key。
