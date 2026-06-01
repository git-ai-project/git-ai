# Git-AI S3 本地打包发布流程

本文档说明如何在本地构建 Git-AI release 二进制，生成带校验和的安装脚本，并上传到公司 S3 下载源。

## 背景

官方发布流程通过 GitHub Actions 构建多平台二进制，并上传到 GitHub Release。内部测试或临时灰度版本可以不上传 GitHub，而是在本地构建后上传到公司 S3，再通过 S3/CloudFront URL 安装。

当前项目已新增本地发布脚本：

```bash
scripts/release-s3-local.sh
```

并且安装脚本已支持 S3 下载源：

- `install.sh`
- `install.ps1`

## 产物结构

本地脚本会生成类似下面的目录：

```text
release/s3-v1.4.9-rebasefix-20260601/
  git-ai-macos-arm64
  SHA256SUMS
  install.sh
  install.ps1
```

上传到 S3 后建议路径为：

```text
s3://your-bucket/git-ai/releases/v1.4.9-rebasefix-20260601/
  git-ai-macos-arm64
  SHA256SUMS
  install.sh
  install.ps1
```

如果通过 CloudFront 或其他 HTTPS 域名暴露，则安装 URL 为：

```text
https://your-download-domain/git-ai/releases/v1.4.9-rebasefix-20260601/install.sh
```

## 前置条件

本地需要安装：

```bash
cargo
aws
```

`cargo` 通常来自 Rust toolchain。脚本会自动尝试加载：

```bash
~/.cargo/env
```

AWS CLI 需要提前配置好凭证，例如：

```bash
aws configure
```

或者使用指定 profile：

```bash
export AWS_PROFILE=your-profile
```

如果使用公司内部 S3 配置，也可以使用 `s3cmd`：

```bash
uv tool run s3cmd -c ~/.s3cfg-prod ls s3://ep-zadig-prod/
```

不要把 `~/.s3cfg-prod` 提交到 Git 仓库。该文件包含 access key 和 secret key，只应保存在本机或公司密钥管理系统中。

## 先做 dry run

第一次建议先执行 dry run。dry run 会构建并生成 release 目录，但不会上传到 S3。

```bash
RELEASE_TAG=v1.4.9-rebasefix-20260601 \
S3_BUCKET=your-bucket \
S3_PREFIX=git-ai/releases \
PUBLIC_BASE_URL=https://your-download-domain/git-ai/releases \
AWS_REGION=ap-southeast-1 \
DRY_RUN=1 \
scripts/release-s3-local.sh
```

使用公司内部 S3 地址时，可以这样 dry run：

```bash
RELEASE_TAG=v1.4.9-rebasefix-20260601 \
S3_BUCKET=ep-zadig-prod \
S3_PREFIX=frontend/packages/tools/git-ai \
PUBLIC_BASE_URL=https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai \
UPLOAD_TOOL=s3cmd \
S3CMD_CONFIG=~/.s3cfg-prod \
CHANNEL=rebasefix \
DRY_RUN=1 \
scripts/release-s3-local.sh
```

生成成功后检查目录：

```bash
ls -la release/s3-v1.4.9-rebasefix-20260601
cat release/s3-v1.4.9-rebasefix-20260601/SHA256SUMS
```

确认 `install.sh` 里已经注入 S3/CloudFront URL：

```bash
rg 'PINNED_VERSION|BASE_URL|EMBEDDED_CHECKSUMS' release/s3-v1.4.9-rebasefix-20260601/install.sh
```

## 正式上传

确认 dry run 没问题后执行：

```bash
RELEASE_TAG=v1.4.9-rebasefix-20260601 \
S3_BUCKET=your-bucket \
S3_PREFIX=git-ai/releases \
PUBLIC_BASE_URL=https://your-download-domain/git-ai/releases \
AWS_REGION=ap-southeast-1 \
scripts/release-s3-local.sh
```

使用公司内部 S3 地址时：

```bash
RELEASE_TAG=v1.4.9-rebasefix-20260601 \
S3_BUCKET=ep-zadig-prod \
S3_PREFIX=frontend/packages/tools/git-ai \
PUBLIC_BASE_URL=https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai \
UPLOAD_TOOL=s3cmd \
S3CMD_CONFIG=~/.s3cfg-prod \
CHANNEL=rebasefix \
scripts/release-s3-local.sh
```

如果需要指定 AWS profile：

```bash
AWS_PROFILE=your-profile \
RELEASE_TAG=v1.4.9-rebasefix-20260601 \
S3_BUCKET=your-bucket \
S3_PREFIX=git-ai/releases \
PUBLIC_BASE_URL=https://your-download-domain/git-ai/releases \
AWS_REGION=ap-southeast-1 \
scripts/release-s3-local.sh
```

脚本会上传到：

```text
s3://your-bucket/git-ai/releases/v1.4.9-rebasefix-20260601/
```

公司内部 S3 示例会上传到：

```text
s3://ep-zadig-prod/frontend/packages/tools/git-ai/v1.4.9-rebasefix-20260601/
```

如果设置了 `CHANNEL=rebasefix`，还会额外上传一份可变频道目录：

```text
s3://ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/
```

上传完成后会打印安装命令：

```bash
curl -fsSL https://your-download-domain/git-ai/releases/v1.4.9-rebasefix-20260601/install.sh | bash
```

Windows 安装命令：

```powershell
irm https://your-download-domain/git-ai/releases/v1.4.9-rebasefix-20260601/install.ps1 | iex
```

公司内部 S3 示例对应安装命令：

```bash
curl -fsSL https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai/v1.4.9-rebasefix-20260601/install.sh | bash
```

如果使用频道安装，用户安装命令可以保持不变：

```bash
curl -fsSL https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/install.sh | bash
```

## 安装验证

在测试机器执行：

```bash
curl -fsSL https://your-download-domain/git-ai/releases/v1.4.9-rebasefix-20260601/install.sh | bash
```

安装完成后验证：

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

如果当前 shell 还没有加载新的 PATH，可以执行：

```bash
source ~/.zshrc
```

或打开一个新的终端窗口。

## 常用参数

`RELEASE_TAG`：必填。内部发布版本号，例如：

```text
v1.4.9-rebasefix-20260601
```

`S3_BUCKET`：必填。S3 bucket 名称。

`PUBLIC_BASE_URL`：必填。用户下载时访问的 HTTPS URL，不包含 `RELEASE_TAG`。

例如：

```text
https://your-download-domain/git-ai/releases
```

`S3_PREFIX`：可选。S3 bucket 内部路径前缀，默认：

```text
git-ai/releases
```

公司内部 S3 示例：

```text
frontend/packages/tools/git-ai
```

`CHANNEL`：可选。可变发布频道名称，例如：

```text
rebasefix
```

设置后，脚本除上传固定版本目录外，还会同步更新：

```text
channels/rebasefix/
```

这样用户可以一直使用固定安装命令：

```bash
curl -fsSL https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/install.sh | bash
```

`UPLOAD_TOOL`：可选。上传工具，默认：

```text
aws
```

如果使用 `s3cmd`：

```text
s3cmd
```

`S3CMD_CONFIG`：可选。`s3cmd` 配置文件路径。公司内部 S3 示例：

```text
~/.s3cfg-prod
```

`AWS_REGION`：可选。传给 AWS CLI 的 region。

`AWS_PROFILE`：可选。使用指定 AWS profile。

`DRY_RUN=1`：可选。只构建本地 release 包，不上传。

`RELEASE_DIR`：可选。自定义本地产物目录。

`SKIP_BUILD=1`：可选。跳过 `cargo build`，直接使用已有的 `target/release/git-ai`。

`PACKAGE_DIR`：可选。使用一个已经包含预构建多平台 binary 的目录作为输入，不在当前机器重新构建。

`REQUIRE_ALL_PLATFORMS=1`：可选。配合 `PACKAGE_DIR` 使用，强制要求目录里存在所有官方平台 binary。

## 平台限制

本地脚本默认只构建当前机器平台的二进制。

例如：

- Apple Silicon Mac：生成 `git-ai-macos-arm64`
- Intel Mac：生成 `git-ai-macos-x64`
- Linux x64：生成 `git-ai-linux-x64`

如果需要完整多平台产物：

```text
git-ai-linux-x64
git-ai-linux-arm64
git-ai-macos-x64
git-ai-macos-arm64
git-ai-windows-x64.exe
git-ai-windows-arm64.exe
```

建议使用新增的 GitHub Actions S3 workflow：

```text
.github/workflows/release-s3.yml
```

或者分别在对应平台机器上执行本地脚本。

## 多平台本地聚合发布

如果不使用 GitHub Actions，又希望发布所有平台版本，可以采用“各平台分别构建和验证，最后在一台机器聚合上传”的方式。

### 1. 各平台分别构建

在 Apple Silicon Mac 上构建：

```bash
cargo build --release --bin git-ai
mkdir -p build/git-ai-all
cp target/release/git-ai build/git-ai-all/git-ai-macos-arm64
```

在 Intel Mac 上构建：

```bash
cargo build --release --bin git-ai
mkdir -p build/git-ai-all
cp target/release/git-ai build/git-ai-all/git-ai-macos-x64
```

在 Linux x64 上构建：

```bash
cargo build --release --bin git-ai
mkdir -p build/git-ai-all
cp target/release/git-ai build/git-ai-all/git-ai-linux-x64
```

在 Linux ARM64 上构建：

```bash
cargo build --release --bin git-ai
mkdir -p build/git-ai-all
cp target/release/git-ai build/git-ai-all/git-ai-linux-arm64
```

在 Windows x64 上构建：

```powershell
cargo build --release --bin git-ai
mkdir build\git-ai-all
copy target\release\git-ai.exe build\git-ai-all\git-ai-windows-x64.exe
```

在 Windows ARM64 上构建：

```powershell
cargo build --release --bin git-ai
mkdir build\git-ai-all
copy target\release\git-ai.exe build\git-ai-all\git-ai-windows-arm64.exe
```

### 2. 每个平台做 smoke test

在对应机器上执行：

```bash
./build/git-ai-all/git-ai-macos-arm64 --version
```

或 Linux：

```bash
./build/git-ai-all/git-ai-linux-x64 --version
```

Windows：

```powershell
.\build\git-ai-all\git-ai-windows-x64.exe --version
```

如果需要更严格验证，可以在对应机器上使用该 binary 做一次本地安装测试：

```bash
GIT_AI_LOCAL_BINARY="$PWD/build/git-ai-all/git-ai-macos-arm64" ./install.sh
git-ai --version
```

Windows：

```powershell
$env:GIT_AI_LOCAL_BINARY = "$PWD\build\git-ai-all\git-ai-windows-x64.exe"
.\install.ps1
git-ai --version
```

只有在对应机器上执行过 smoke test，才能确认该平台版本实际可运行。单台 Mac 无法验证 Windows/Linux binary 的运行时可用性。

### 3. 汇总到同一个目录

把各平台产物复制到同一台发布机器上的同一个目录，例如：

```text
build/git-ai-all/
  git-ai-linux-x64
  git-ai-linux-arm64
  git-ai-macos-x64
  git-ai-macos-arm64
  git-ai-windows-x64.exe
  git-ai-windows-arm64.exe
```

### 4. dry run 聚合发布

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

### 5. 正式上传所有平台版本

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

这个命令会生成包含所有平台 checksum 的安装脚本，并上传：

```text
s3://ep-zadig-prod/frontend/packages/tools/git-ai/v1.4.9-rebasefix-20260601/
s3://ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/
```

用户仍然使用固定频道安装命令：

```bash
curl -fsSL https://s3.it.lixiangoa.com/ep-zadig-prod/frontend/packages/tools/git-ai/channels/rebasefix/install.sh | bash
```

## 版本号说明

`git-ai --version` 的显示值来自项目版本配置，不一定等于内部 S3 路径中的 `RELEASE_TAG`。

因此，内部测试版本建议通过 S3 路径区分，例如：

```text
v1.4.9-rebasefix-20260601
```

如果希望 `git-ai --version` 也显示内部版本，需要额外修改项目版本号或版本生成逻辑。

## 故障排查

如果提示找不到 `cargo`：

```bash
source ~/.cargo/env
```

如果提示找不到 `aws`：

```bash
brew install awscli
```

如果上传失败，先确认当前身份：

```bash
aws sts get-caller-identity
```

确认 S3 权限：

```bash
aws s3 ls s3://your-bucket/git-ai/releases/
```

如果安装时 checksum 失败，说明 S3 上的二进制和 `install.sh` 内嵌 checksum 不匹配。重新执行发布脚本并上传完整目录即可。
