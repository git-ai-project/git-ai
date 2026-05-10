# macOS Package Installer

A deterministic `.pkg` installer for git-ai. Uses only `pkgbuild` (ships with macOS).

## What it does

- Installs `git-ai` and a `git` shim into `/Library/git-ai/bin/`
- Prepends the install directory to the system PATH via `/etc/paths`
- Supports silent enterprise deployment: `sudo installer -pkg git-ai.pkg -target /`
- Compatible with MDM tools (Jamf, Kandji, Mosyle, etc.)

## What it does NOT do (by design)

- No daemon startup
- No login prompts
- No shell profile (`.bashrc`/`.zshrc`) modifications
- No searching for existing Git installations

All dynamic behavior is deferred to `git-ai` runtime.

## Build requirements

- macOS with `pkgbuild` (included in Xcode Command Line Tools)
- A compiled `git-ai` binary

## Usage

```bash
# Build for Apple Silicon
./build-pkg.sh --binary ../../target/aarch64-apple-darwin/release/git-ai --arch arm64

# Build for Intel
./build-pkg.sh --binary ../../target/x86_64-apple-darwin/release/git-ai --arch x64

# Explicit version override
./build-pkg.sh --binary path/to/git-ai --version 1.4.6 --arch arm64

# Custom output path
./build-pkg.sh --binary path/to/git-ai --output ~/Desktop/git-ai.pkg
```

## Architecture

```
build-pkg.sh
  │
  ├─ Stage binaries (copy git-ai, duplicate as git)
  ├─ Set permissions (755)
  └─ pkgbuild → .pkg with postinstall script

postinstall
  └─ Prepend /Library/git-ai/bin to /etc/paths
```

## Enterprise deployment

```bash
# Silent install
sudo installer -pkg git-ai-macos-arm64.pkg -target /

# MDM (Jamf example)
# Upload the .pkg and deploy to target machines
```
