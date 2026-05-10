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
$GitShim = Join-Path $InstallDir 'git.exe'

function Test-FileAvailable {
    param([Parameter(Mandatory)][string]$Path)
    try {
        $stream = [System.IO.File]::Open($Path, 'Open', 'Write', 'None')
        $stream.Close()
        return $true
    } catch {
        return $false
    }
}

function Stop-GitAiDaemon {
    param([Parameter(Mandatory)][string]$GitAiExe, [switch]$Hard)
    if (-not (Test-Path -LiteralPath $GitAiExe)) { return $false }
    $shutdownArgs = @('bg', 'shutdown')
    if ($Hard) { $shutdownArgs += '--hard' }
    try { & $GitAiExe @shutdownArgs *> $null; return ($LASTEXITCODE -eq 0) } catch { return $false }
}

function Wait-ForFileAvailable {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$GitAiExe,
        [int]$MaxWaitSeconds = 30,
        [int]$RetryIntervalSeconds = 2,
        [int]$HardKillAfterSeconds = 10
    )
    [void](Stop-GitAiDaemon -GitAiExe $GitAiExe)
    $elapsed = 0
    while ($elapsed -lt $MaxWaitSeconds) {
        if (Test-FileAvailable -Path $Path) { return $true }
        if ($elapsed -ge $HardKillAfterSeconds) {
            [void](Stop-GitAiDaemon -GitAiExe $GitAiExe -Hard)
            $dir = Split-Path $GitAiExe -Parent
            $targets = @(
                ([IO.Path]::GetFullPath($GitAiExe)).ToLowerInvariant(),
                ([IO.Path]::GetFullPath((Join-Path $dir 'git.exe'))).ToLowerInvariant()
            )
            @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
                Where-Object { $_.ProcessId -ne $PID -and $_.ExecutablePath -and
                    ($targets -contains $_.ExecutablePath.ToLowerInvariant()) }) |
                ForEach-Object { try { Stop-Process -Id $_.ProcessId -Force } catch { } }
        }
        if ($elapsed -eq 0) {
            Write-Host "Waiting for file to be available: $Path" -ForegroundColor Yellow
        }
        Start-Sleep -Seconds $RetryIntervalSeconds
        $elapsed += $RetryIntervalSeconds
    }
    return $false
}

# Atomically replace $DstPath with $SrcPath, stopping the daemon first if needed.
# Uses Remove + Move rather than Move-Item -Force because Move-Item -Force raises
# ERROR_ALREADY_EXISTS when the destination is locked even after unlocking.
function Install-Binary {
    param(
        [Parameter(Mandatory)][string]$SrcPath,
        [Parameter(Mandatory)][string]$DstPath,
        [Parameter(Mandatory)][string]$GitAiExe
    )
    $tmpPath = "$DstPath.tmp.$PID"
    Copy-Item -Force -Path $SrcPath -Destination $tmpPath
    if (Test-Path -LiteralPath $DstPath) {
        if (-not (Wait-ForFileAvailable -Path $DstPath -GitAiExe $GitAiExe)) {
            Remove-Item -Force -ErrorAction SilentlyContinue $tmpPath
            throw "Timeout waiting for '$DstPath' to be available. Close any running git-ai processes and try again."
        }
        Remove-Item -Force -Path $DstPath
    }
    Move-Item -Path $tmpPath -Destination $DstPath
}

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
    if (-not $onPath) { $needsInstall = $true }
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

# Replace git-ai.exe, stopping the daemon first if it is running
Write-Host "Installing binary to $GitAiExe..."
Install-Binary -SrcPath "target\$BuildType\git-ai.exe" -DstPath $GitAiExe -GitAiExe $GitAiExe

# Keep the git.exe shim in sync with the updated binary
if (Test-Path -LiteralPath $GitShim) {
    Write-Host 'Updating git.exe shim...'
    Install-Binary -SrcPath $GitAiExe -DstPath $GitShim -GitAiExe $GitAiExe
}

# Run install hooks
Write-Host 'Running install hooks...'
& $GitAiExe install
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Install developer pre-commit hook (for format/lint checks)
if (Test-Path -LiteralPath "scripts\pre-commit") {
    Write-Host 'Installing developer pre-commit hook...'
    $hookPath = ".git\hooks\pre-commit"
    # Windows doesn't support symlinks well, so copy the file
    Copy-Item -Force "scripts\pre-commit" $hookPath
    Write-Host '✅ Pre-commit hook installed (bypass with --no-verify)'
}

Write-Host 'Done!'
