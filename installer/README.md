# Installers

Deterministic, enterprise-ready installers for git-ai. Zero third-party dependencies — each platform uses only native tooling.

## Platform Matrix

| Platform | Format | Build Tool | Install Path | Silent Install |
|----------|--------|-----------|--------------|----------------|
| Windows | `.msi` | makecab + msidb + COM | `C:\Program Files\git-ai\bin\` | `msiexec /i git-ai.msi /qn` |
| macOS | `.pkg` | pkgbuild | `/Library/git-ai/bin/` | `sudo installer -pkg git-ai.pkg -target /` |
| Linux (Debian/Ubuntu) | `.deb` | dpkg-deb | `/usr/lib/git-ai/bin/` | `sudo dpkg -i git-ai.deb` |
| Linux (RHEL/Fedora) | `.rpm` | rpmbuild | `/usr/lib/git-ai/bin/` | `sudo rpm -i git-ai.rpm` |
| macOS/Linux | Homebrew | brew | Homebrew `bin/` | `brew install git-ai-project/tap/git-ai` |
| NixOS/Linux/macOS | Nix flake | nix | Nix store | `nix profile install github:git-ai-project/git-ai` |

## PATH Precedence

Each installer ensures `git-ai` takes priority over the system `git`:

- **Windows**: MSI prepends to the system `PATH` environment variable
- **macOS**: pkg prepends to `/etc/paths` (read by `path_helper(8)`)
- **Linux**: deb/rpm add `/etc/profile.d/git-ai.sh` which exports the path
- **Nix**: The flake's git wrapper uses `exec -a git` to set argv[0], handled by Nix store PATH

## Enterprise Rollout

### Windows (Group Policy / SCCM / Intune)

```powershell
# Silent install (no UI, no reboot)
msiexec /i git-ai-windows-x64.msi /qn /norestart

# Silent uninstall
msiexec /x git-ai-windows-x64.msi /qn

# Upgrade (MSI handles this automatically via UpgradeCode)
msiexec /i git-ai-windows-x64-new-version.msi /qn
```

The MSI uses a shared `UpgradeCode`, so deploying a new version automatically removes the old one. No custom scripts needed.

### macOS (Jamf / Kandji / Mosyle)

```bash
# Silent install
sudo installer -pkg git-ai-macos-arm64.pkg -target /

# Uninstall
sudo rm -rf /Library/git-ai
sudo sed -i '' '\|/Library/git-ai/bin|d' /etc/paths
```

Upload the `.pkg` directly to your MDM. No pre/post scripts required — the package handles PATH configuration via its built-in postinstall script.

### Linux (Ansible / Puppet / Chef)

```bash
# Debian/Ubuntu
sudo dpkg -i git-ai_1.4.6_amd64.deb

# RHEL/Fedora/CentOS
sudo rpm -i git-ai-1.4.6-1.x86_64.rpm

# Uninstall (deb)
sudo dpkg -r git-ai

# Uninstall (rpm)
sudo rpm -e git-ai
```

### Homebrew (developer workstations)

```bash
brew install git-ai-project/tap/git-ai
brew upgrade git-ai
brew uninstall git-ai
```

### Nix (NixOS / Home Manager / nix profile)

The repository includes a comprehensive `flake.nix` at the project root with NixOS modules, Home Manager modules, and an overlay.

#### NixOS module (system-wide)

```nix
# flake.nix
{
  inputs.git-ai.url = "github:git-ai-project/git-ai";

  outputs = { self, nixpkgs, git-ai, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        git-ai.nixosModules.default
        {
          programs.git-ai = {
            enable = true;
            setGitAlias = true;  # places git-ai's git wrapper before system git in PATH
            settings = {
              promptStorage = "local";
              disableAutoUpdates = true;  # managed by Nix
            };
          };
        }
      ];
    };
  };
}
```

#### Home Manager (per-user)

```nix
# home.nix
{
  imports = [ inputs.git-ai.homeManagerModules.default ];

  programs.git-ai = {
    enable = true;
    package = inputs.git-ai.packages.${system}.minimal;  # without git wrapper
    settings.promptStorage = "notes";
  };
}
```

#### Direct install (developer workstations)

```bash
# Install to user profile
nix profile install github:git-ai-project/git-ai

# Run without installing
nix run github:git-ai-project/git-ai -- status

# Upgrade
nix profile upgrade git-ai

# Uninstall
nix profile remove git-ai
```

#### Overlay (importing into other flakes)

```nix
{
  inputs.git-ai.url = "github:git-ai-project/git-ai";

  outputs = { self, nixpkgs, git-ai, ... }: {
    # Makes pkgs.git-ai and pkgs.git-ai-unwrapped available
    nixpkgs.overlays = [ git-ai.overlays.default ];
  };
}
```

Package variants: `default` (with git wrapper + git-og), `minimal` (without git wrapper), `unwrapped` (bare binary only).

## Design Principles

1. **No runtime actions at install time** — installers only place files and update PATH. No daemon startup, no login prompts, no network calls.
2. **Deterministic** — same inputs produce identical outputs. GUIDs are derived from version + architecture, not random.
3. **Clean uninstall** — every installer removes what it placed. No orphaned files or registry entries.
4. **No elevated runtime** — installation requires admin/root, but git-ai itself runs as the current user.
5. **Upgrade-safe** — all formats support in-place upgrades without manual uninstall.

## Building Locally

```bash
# Windows MSI (run on Windows with Windows SDK installed)
cd windows
powershell -File build-msi.ps1 -BinaryPath path\to\git-ai.exe -Architecture x64

# macOS pkg (run on macOS)
cd macos
./build-pkg.sh --binary path/to/git-ai --arch arm64

# Linux deb
cd linux
./build-deb.sh --binary path/to/git-ai --arch amd64

# Linux rpm
cd linux
./build-rpm.sh --binary path/to/git-ai --arch x86_64

# Homebrew formula (generate from release checksums)
cd homebrew
./update-formula.sh --version 1.4.6 --repo git-ai-project/git-ai --checksums path/to/SHA256SUMS
```

## CI Integration

All installers are built automatically as part of the release workflow. The pipeline:

1. **Build** — compiles binaries for all 6 targets
2. **Package** — builds MSI, pkg, deb, rpm in parallel on appropriate runners
3. **Release** — checksums, attests, and publishes everything to GitHub Releases
