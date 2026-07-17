# MSI/PKG release runbook

This runbook covers the Windows MSI and macOS PKG beta installers. They are first-install bootstrap packages; the existing user-level update flow remains the source of subsequent updates.

## Scope and guardrails

- Ship only Windows MSI and macOS PKG installers in this release path.
- The Windows MSI is per-user only. It installs to the current user's local app data and updates that user's `PATH`; there is no all-users mode in this release.
- The macOS PKG writes its bootstrap binary with installer privileges, then runs user setup as the active console user. It must not create root-owned state in that user's home directory.
- Do not add a Git shim or wrapper to either package.
- The release body must contain the MSI/PKG beta warning. Do not put the warning in the release title or asset names.

## One-time repository setup

Complete these settings before the first release run:

1. Keep signing and notarization credentials in the GitHub `release` environment, with no required reviewers. A reviewer-protected `release` environment would prompt separately for every signing job.
2. Create a `release-approval` environment with exactly one required reviewer from Sasha or Aidan. It is an approval gate only and contains no secrets or variables.
3. Require the workflow to authorize the triggering user before the approval gate. Only trusted release operators may run a workflow that can access the `release` secrets.
4. Confirm the repository Actions policy permits the pinned Azure signing actions used by `.github/workflows/release.yml`.
5. Confirm the release workflow can build both `git-ai-windows-x64.msi` and `git-ai-windows-arm64.msi`.

The repository administrator must create the GitHub environment and its protection rule. The workflow cannot create or protect environments itself.

## Run the release workflow

Open **Actions → Release Build → Run workflow** and use the appropriate inputs:

| Release kind | `dry_run` | `release_production` | Approval |
| --- | --- | --- | --- |
| Validation only | `true` | `false` | None |
| Pre-release | `false` | `false` | One `release-approval` review |
| Production | `false` | `true` | One `release-approval` review; run from `main` |

For every non-dry release, approve `Release approval` once at the start. The dependent build, signing, package, test, and publishing jobs use the existing `release` environment for credentials without further approval prompts.

## What the workflow publishes

1. The workflow builds the regular binaries, x64 and ARM64 MSIs, and Intel and Apple Silicon PKGs.
2. It signs the MSIs, validates an MSI install on Windows, then creates the core GitHub release with the automatic MSI/PKG beta warning in the release body.
3. macOS notarization runs separately. The core release does not depend on the PKG packaging or notarization jobs, so notarization cannot block an emergency release.
4. After notarization and PKG validation succeed, `Publish macOS PKG installers` attaches both PKGs and `PKG-SHA256SUMS` to that same release.

If PKG notarization fails or is delayed, investigate or retry the PKG publishing path. Do not recreate the core release just to wait for notarization.

## Stable release asset check

After the first stable release from `main` completes, verify that these customer-facing URLs download the matching release assets before publishing or merging customer documentation:

- `https://github.com/git-ai-project/git-ai/releases/latest/download/git-ai-windows-x64.msi`
- `https://github.com/git-ai-project/git-ai/releases/latest/download/git-ai-windows-arm64.msi`
- `https://github.com/git-ai-project/git-ai/releases/latest/download/git-ai-macos-arm64.pkg`
- `https://github.com/git-ai-project/git-ai/releases/latest/download/git-ai-macos-x64.pkg`

For each downloaded PKG, run:

```bash
pkgutil --check-signature git-ai-macos-<arch>.pkg
xcrun stapler validate git-ai-macos-<arch>.pkg
spctl --assess --type install --verbose=4 git-ai-macos-<arch>.pkg
```

For each MSI, verify the signature on a Windows test machine:

```powershell
Get-AuthenticodeSignature .\git-ai-windows-<arch>.msi
```

## Managed API configuration

The MSI accepts `API_BASE` and `API_KEY` through `msiexec` and persists them for the installing user. The PKG has no equivalent installer-property interface. For a managed macOS install, run `git-ai setup-package --manager pkg --api-base ... --api-key ...` as the target developer user after the PKG installation. Do not use `sudo` for that configuration step, and use a disposable test key for validation.

## Before production

Run a non-production release first, then complete the manual validation matrix:

- macOS: install the PKG with installer privilege and verify normal-user setup, no root-owned `~/.git-ai` state, the CLI, and the daemon runtime selection.
- Windows ARM64: install the ARM64 MSI in UTM on Apple Silicon as a standard user and as an Administrator; verify that the default per-user install does not create Administrator-owned user state.
- Windows x64: repeat the MSI checks on a physical Windows machine.
- On both platforms, install the package, run the existing update flow, and confirm the CLI and daemon use the user runtime after the update.

Record the results with the release. Production is ready only after the non-production release and this matrix pass.
