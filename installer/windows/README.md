# Windows MSI Installer

A deterministic, zero-dependency MSI installer for git-ai. Uses only native Windows SDK tools — no WiX, no third-party crates.

## What it does

- Installs `git-ai.exe` and a `git.exe` shim into `C:\Program Files\git-ai\bin\`
- Prepends the install directory to the system PATH (before any existing Git)
- Supports silent enterprise deployment: `msiexec /i git-ai.msi /qn`
- Supports clean uninstall (removes binaries and PATH entry)
- Supports in-place upgrades via shared UpgradeCode

## What it does NOT do (by design)

- No custom actions or PowerShell execution during install
- No daemon startup
- No login prompts
- No `.bashrc` modifications
- No searching for existing Git installations

All dynamic behavior is deferred to `git-ai.exe` runtime.

## Build requirements

- Windows SDK (for `msidb.exe` only)
- `makecab.exe` (built into Windows)
- `WindowsInstaller.Installer` COM object (built into Windows — used for Summary Information Stream)
- A compiled `git-ai.exe` binary

## Usage

```powershell
# Build for x64
.\build-msi.ps1 -BinaryPath "..\..\target\x86_64-pc-windows-msvc\release\git-ai.exe" -Architecture x64

# Build for ARM64
.\build-msi.ps1 -BinaryPath "..\..\target\aarch64-pc-windows-msvc\release\git-ai.exe" -Architecture arm64

# Explicit version override
.\build-msi.ps1 -BinaryPath "path\to\git-ai.exe" -Version "1.4.6" -Architecture x64

# Custom output path
.\build-msi.ps1 -BinaryPath "path\to\git-ai.exe" -OutputPath "C:\out\git-ai-x64.msi"
```

## IDT schema

The `.idt` files in `idt/` define the MSI database schema as tab-separated text. These are the standard Windows Installer table format:

| File | Purpose |
|------|---------|
| `Property.idt` | Product metadata, ALLUSERS=1 for machine-wide |
| `Directory.idt` | Filesystem layout (ProgramFiles64\git-ai\bin) |
| `Component.idt` | Installation unit (64-bit attribute) |
| `File.idt` | Binary declarations |
| `Feature.idt` | Feature tree (single "Complete" feature) |
| `FeatureComponents.idt` | Feature-to-component mapping |
| `Media.idt` | Cabinet file reference |
| `Environment.idt` | System PATH prepend |
| `Upgrade.idt` | Major upgrade detection |
| `InstallExecuteSequence.idt` | Install action ordering |
| `AdminExecuteSequence.idt` | Admin install action ordering |

## Architecture

```
build-msi.ps1
  │
  ├─ Stage binaries (copy git-ai.exe, duplicate as git.exe)
  ├─ Generate deterministic ProductCode GUID from version+arch
  ├─ Patch IDT files with version/size data
  ├─ makecab.exe → git-ai.cab (MSZIP compressed)
  ├─ msidb.exe → import IDT tables + inject cab
  └─ WindowsInstaller.Installer COM → stamp Summary Information Stream
```

## Determinism

- **ProductCode**: SHA-256 derived from version + architecture (same inputs = same GUID)
- **UpgradeCode**: Static across all versions (enables upgrade detection)
- **ComponentId**: Static (stable component identity for repair/patching)
- **File sizes**: Patched at build time from actual binaries
