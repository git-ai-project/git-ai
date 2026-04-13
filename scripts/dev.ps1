$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Parse arguments
$BuildType = 'debug'
if ($args.Count -gt 0 -and $args[0] -eq '--release') {
    $BuildType = 'release'
}

$InstallDir = Join-Path $HOME '.git-ai\bin'
$ConfigPath = Join-Path $HOME '.git-ai\config.json'
$GitAiExe = Join-Path $InstallDir 'git-ai.exe'

# Run production installer if ~/.git-ai isn't set up or ~/.git-ai/bin isn't on PATH
$needsInstall = $false
if (-not (Test-Path -LiteralPath $InstallDir) -or
    -not (Test-Path -LiteralPath $ConfigPath)) {
    $needsInstall = $true
}

if (-not $needsInstall) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $installDirNorm = ([IO.Path]::GetFullPath($InstallDir)).TrimEnd('\').ToLowerInvariant()
    $onPath = $false
    foreach ($entry in (("$userPath;$machinePath") -split ';')) {
        if (-not $entry.Trim()) { continue }
        try {
            if (([IO.Path]::GetFullPath($entry.Trim())).TrimEnd('\').ToLowerInvariant() -eq $installDirNorm) {
                $onPath = $true
                break
            }
        } catch { }
    }
    if (-not $onPath) {
        $needsInstall = $true
    }
}

if ($needsInstall) {
    Write-Host 'Running git-ai installer...'
    & (Join-Path $PSScriptRoot '..\install.ps1')
}

# Build the binary
Write-Host "Building $BuildType binary..."
if ($BuildType -eq 'release') {
    cargo build --release
} else {
    cargo build
}
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Install binary via temp file + atomic Move-Item to avoid Windows file-lock issues
# with running processes reusing the same on-disk binary (mirrors the macOS inode
# workaround in dev.sh but needed on Windows for a different reason: antivirus/
# Defender scanning the file while it's being replaced).
Write-Host "Installing binary to $GitAiExe..."
$tmpBin = "$GitAiExe.tmp.$PID"
Copy-Item -Force -Path "target\$BuildType\git-ai.exe" -Destination $tmpBin
Move-Item -Force -Path $tmpBin -Destination $GitAiExe

# Keep the git.exe shim in sync with the updated binary
$gitShim = Join-Path $InstallDir 'git.exe'
if (Test-Path -LiteralPath $gitShim) {
    Write-Host 'Updating git.exe shim...'
    $tmpShim = "$gitShim.tmp.$PID"
    Copy-Item -Force -Path $GitAiExe -Destination $tmpShim
    Move-Item -Force -Path $tmpShim -Destination $gitShim
}

# Run install hooks
Write-Host 'Running install hooks...'
& $GitAiExe install
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host 'Done!'
