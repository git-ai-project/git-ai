$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# ============================================================
# easylife-ai local offline installer for Windows
# Installs from binaries in the same directory as this script.
# No network access required.
# ============================================================

function Write-ErrorAndExit {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host "Error: $Message" -ForegroundColor Red
    exit 1
}

function Write-Success {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host $Message -ForegroundColor Green
}

function Write-Warning {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host $Message -ForegroundColor Yellow
}

function Normalize-PathString {
    param([Parameter(Mandatory = $true)][string]$Path)
    try {
        return ([IO.Path]::GetFullPath($Path.Trim())).TrimEnd('\').ToLowerInvariant()
    } catch {
        return ($Path.Trim()).TrimEnd('\').ToLowerInvariant()
    }
}

function Test-FileAvailable {
    param([Parameter(Mandatory = $true)][string]$Path)
    try {
        $stream = [System.IO.File]::Open($Path, 'Open', 'Write', 'None')
        $stream.Close()
        return $true
    } catch {
        return $false
    }
}

function Stop-GitAiBackgroundService {
    param(
        [Parameter(Mandatory = $true)][string]$GitAiExe,
        [Parameter(Mandatory = $false)][switch]$Hard
    )
    if (-not (Test-Path -LiteralPath $GitAiExe)) {
        return $false
    }
    $args = @('bg', 'shutdown')
    if ($Hard) {
        $args += '--hard'
    }
    try {
        & $GitAiExe @args *> $null
        return $LASTEXITCODE -eq 0
    } catch {
        return $false
    }
}

function Get-GitAiManagedProcesses {
    param([Parameter(Mandatory = $true)][string]$InstallDir)
    $targetPaths = @(
        (Join-Path $InstallDir 'git-ai.exe'),
        (Join-Path $InstallDir 'git.exe'),
        (Join-Path $InstallDir 'easylife-ai.exe')
    )
    $normalizedTargets = $targetPaths | ForEach-Object { Normalize-PathString $_ }
    $processes = Get-Process -ErrorAction SilentlyContinue | Where-Object {
        try {
            $procPath = $_.Path
            if ([string]::IsNullOrWhiteSpace($procPath)) { return $false }
            $normalizedProc = Normalize-PathString $procPath
            return $normalizedTargets -contains $normalizedProc
        } catch {
            return $false
        }
    }
    return $processes
}

# Detect architecture
$arch = if ([Environment]::Is64BitOperatingSystem) {
    if ([Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE') -eq 'ARM64') {
        'arm64'
    } else {
        'x64'
    }
} else {
    Write-ErrorAndExit "32-bit Windows is not supported"
}

# Script directory (where binaries live)
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$binaryName = "easylife-ai-windows-$arch.exe"
$binaryPath = Join-Path $scriptDir $binaryName

if (-not (Test-Path -LiteralPath $binaryPath)) {
    Write-ErrorAndExit "Binary not found: $binaryPath`nMake sure $binaryName is in the same directory as this script."
}

Write-Host "Installing easylife-ai from $binaryPath..."

# Detect standard git
$stdGitPath = $null
$gitCandidates = @(
    (Get-Command git -ErrorAction SilentlyContinue).Source,
    'C:\Program Files\Git\cmd\git.exe',
    'C:\Program Files (x86)\Git\cmd\git.exe'
)

foreach ($candidate in $gitCandidates) {
    if ([string]::IsNullOrWhiteSpace($candidate)) { continue }
    if ($candidate -like '*git-ai*') { continue }
    if (Test-Path -LiteralPath $candidate) {
        try {
            & $candidate --version *> $null
            if ($LASTEXITCODE -eq 0) {
                $stdGitPath = $candidate
                break
            }
        } catch {}
    }
}

if (-not $stdGitPath) {
    $configJson = Join-Path $HOME '.git-ai\config.json'
    if (Test-Path -LiteralPath $configJson) {
        try {
            $cfg = Get-Content -LiteralPath $configJson -Raw | ConvertFrom-Json
            if ($cfg.git_path -and ($cfg.git_path -notlike '*git-ai*')) {
                if (Test-Path -LiteralPath $cfg.git_path) {
                    & $cfg.git_path --version *> $null
                    if ($LASTEXITCODE -eq 0) {
                        $stdGitPath = $cfg.git_path
                    }
                }
            }
        } catch {}
    }
}

if (-not $stdGitPath) {
    Write-ErrorAndExit "Could not detect a standard git binary. Please ensure Git is installed."
}

$installDir = Join-Path $HOME '.git-ai\bin'
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

$finalExe = Join-Path $installDir 'git-ai.exe'
$easylifeExe = Join-Path $installDir 'easylife-ai.exe'
$gitShimExe = Join-Path $installDir 'git.exe'
$gitOgCmd = Join-Path $installDir 'git-og.cmd'

# Shutdown background service if running
if (Test-Path -LiteralPath $finalExe) {
    Write-Host 'Shutting down background service...'
    $shutdownOk = Stop-GitAiBackgroundService -GitAiExe $finalExe
    if (-not $shutdownOk) {
        Stop-GitAiBackgroundService -GitAiExe $finalExe -Hard | Out-Null
    }
    Start-Sleep -Milliseconds 500
}

# Kill any remaining processes
$remainingProcs = Get-GitAiManagedProcesses -InstallDir $installDir
if ($remainingProcs) {
    Write-Host 'Stopping remaining processes...'
    $remainingProcs | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

# Wait for files to be available
$maxWait = 10
$waited = 0
while ($waited -lt $maxWait) {
    $allAvailable = $true
    foreach ($f in @($finalExe, $easylifeExe, $gitShimExe)) {
        if ((Test-Path -LiteralPath $f) -and (-not (Test-FileAvailable $f))) {
            $allAvailable = $false
            break
        }
    }
    if ($allAvailable) { break }
    Start-Sleep -Milliseconds 500
    $waited++
}

# Copy binaries
Copy-Item -Force -Path $binaryPath -Destination $finalExe
Copy-Item -Force -Path $binaryPath -Destination $easylifeExe
Copy-Item -Force -Path $binaryPath -Destination $gitShimExe

# Create git-og.cmd shim
$gitOgContent = "@echo off`r`n`"$stdGitPath`" %*"
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($gitOgCmd, $gitOgContent, $utf8NoBom)

Write-Success "Installed to $installDir"

# Write config.json if not present
$configDir = Join-Path $HOME '.git-ai'
$configJsonPath = Join-Path $configDir 'config.json'
New-Item -ItemType Directory -Force -Path $configDir | Out-Null

if (-not (Test-Path -LiteralPath $configJsonPath)) {
    $cfg = @{
        git_path = $stdGitPath
        feature_flags = @{
            async_mode = $true
        }
    } | ConvertTo-Json -Depth 3 -Compress
    [System.IO.File]::WriteAllText($configJsonPath, $cfg, $utf8NoBom)
}

# Install hooks
Write-Host 'Setting up IDE/agent hooks...'
try {
    & $finalExe install-hooks *> $null
    if ($LASTEXITCODE -eq 0) {
        Write-Success 'IDE/agent hooks configured'
    } else {
        Write-Warning 'Failed to set up IDE/agent hooks. Run "git-ai install-hooks" manually.'
    }
} catch {
    Write-Warning 'Failed to set up IDE/agent hooks. Run "git-ai install-hooks" manually.'
}

# Update PATH (User scope)
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathEntries = if ($userPath) { $userPath.Split(';') } else { @() }
$normalizedEntries = $pathEntries | ForEach-Object { Normalize-PathString $_ }
$normalizedInstallDir = Normalize-PathString $installDir

if ($normalizedEntries -notcontains $normalizedInstallDir) {
    $newPath = if ($userPath) { "$installDir;$userPath" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = "$installDir;$env:Path"
    Write-Success "Added $installDir to User PATH"
} else {
    Write-Success "PATH already contains $installDir"
}

# Configure Git Bash if present
$gitBashConfigured = $false
$gitBashAlreadyConfigured = $false

$bashrcPath = Join-Path $HOME '.bashrc'
$bashProfilePath = Join-Path $HOME '.bash_profile'

$targetBashConfig = if (Test-Path -LiteralPath $bashrcPath) {
    $bashrcPath
} else {
    $bashProfilePath
}

try {
    $pathLine = "export PATH=`"$($installDir -replace '\\', '/'):`$PATH`""
    $existingContent = if (Test-Path -LiteralPath $targetBashConfig) {
        Get-Content -LiteralPath $targetBashConfig -Raw -ErrorAction SilentlyContinue
    } else {
        ''
    }

    if ($existingContent -notlike "*$installDir*") {
        $newContent = if ($existingContent) {
            "$existingContent`n`n# Added by easylife-ai installer on $(Get-Date -Format 'yyyy-MM-dd')`n$pathLine`n"
        } else {
            "# Added by easylife-ai installer on $(Get-Date -Format 'yyyy-MM-dd')`n$pathLine`n"
        }
        [System.IO.File]::WriteAllText($targetBashConfig, $newContent, $utf8NoBom)
        $gitBashConfigured = $true
    } else {
        $gitBashAlreadyConfigured = $true
    }
} catch {
    Write-Host "Warning: Failed to configure Git Bash: $($_.Exception.Message)" -ForegroundColor Yellow
}

if ($gitBashConfigured) {
    Write-Success "Successfully configured Git Bash ($targetBashConfig)"
} elseif ($gitBashAlreadyConfigured) {
    Write-Success "Git Bash already configured ($targetBashConfig)"
}

Write-Host ''
Write-Host 'Installation complete!' -ForegroundColor Green
Write-Host 'Close and reopen your terminal and IDE sessions to use easylife-ai.' -ForegroundColor Yellow
Write-Host ''
Write-Host 'You can now run:' -ForegroundColor Cyan
Write-Host '  easylife-ai --version'
Write-Host '  git-ai --version'
