# Linux Package Installers

Deterministic `.deb` and `.rpm` package builders for git-ai. Uses only `dpkg-deb` and `rpmbuild` respectively — no third-party tools.

## What they do

- Install `git-ai` and a `git` shim into `/usr/lib/git-ai/bin/`
- Add `/etc/profile.d/git-ai.sh` to prepend to PATH for all users
- Support silent enterprise deployment
- Clean uninstall removes binaries and PATH entry

## What they do NOT do (by design)

- No daemon startup
- No login prompts
- No user-level shell profile modifications

## Build requirements

- **deb**: `dpkg-deb` (included in Debian/Ubuntu)
- **rpm**: `rpmbuild` (from `rpm-build` package on RHEL/Fedora)
- A compiled `git-ai` binary (statically linked with musl)

## Usage

### .deb (Debian, Ubuntu)

```bash
# Build for amd64
./build-deb.sh --binary ../../target/x86_64-unknown-linux-musl/release/git-ai --arch amd64

# Build for arm64
./build-deb.sh --binary ../../target/aarch64-unknown-linux-musl/release/git-ai --arch arm64

# Install
sudo dpkg -i build/git-ai_1.4.6_amd64.deb
```

### .rpm (RHEL, Fedora, CentOS)

```bash
# Build for x86_64
./build-rpm.sh --binary ../../target/x86_64-unknown-linux-musl/release/git-ai --arch x86_64

# Build for aarch64
./build-rpm.sh --binary ../../target/aarch64-unknown-linux-musl/release/git-ai --arch aarch64

# Install
sudo rpm -i build/git-ai-1.4.6-1.x86_64.rpm
```
