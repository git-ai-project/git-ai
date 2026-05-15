# Usage: irm https://raw.githubusercontent.com/git-ai-inc/git-ai/main/scripts/install.ps1 | iex
#
# Installs the latest git-ai release for Windows.
# Downloads the Windows zip from GitHub releases.
# Extracts to $env:USERPROFILE\.git-ai\bin\
# Adds to PATH if not already there.
# Runs `git-ai install`.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# --- Configuration ---
$Repo = 'git-ai-inc/git-ai'
$InstallDir = Join-Path $env:USERPROFILE '.git-ai\bin'
$GithubBase = "https://github.com/$Repo/releases"

# --- Helpers ---
function Write-ErrorAndExit {
    param([string]$Message)
    Write-Host "error: $Message" -ForegroundColor Red
    exit 1
}

function Write-Warn {
    param([string]$Message)
    Write-Host "warning: $Message" -ForegroundColor Yellow
}

function Write-Ok {
    param([string]$Message)
    Write-Host $Message -ForegroundColor Green
}

# --- Detect architecture ---
function Get-Architecture {
    try {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
        switch ($arch) {
            'X64'   { return 'x86_64' }
            'Arm64' { return 'aarch64' }
            default { return $null }
        }
    } catch {
        $pa = $env:PROCESSOR_ARCHITECTURE
        if ($pa -match 'AMD64') { return 'x86_64' }
        elseif ($pa -match 'ARM64') { return 'aarch64' }
        else { return $null }
    }
}

# --- Main ---
$Arch = Get-Architecture
if (-not $Arch) {
    Write-ErrorAndExit "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE"
}

$Target = "$Arch-pc-windows-msvc"
$ZipName = "git-ai-$Target.zip"

# Determine version
$Version = if ($env:GIT_AI_VERSION) { $env:GIT_AI_VERSION } else { 'latest' }

if ($Version -eq 'latest') {
    $DownloadUrl = "$GithubBase/latest/download/$ZipName"
    $ChecksumUrl = "$GithubBase/latest/download/SHA256SUMS"
} else {
    $DownloadUrl = "$GithubBase/download/$Version/$ZipName"
    $ChecksumUrl = "$GithubBase/download/$Version/SHA256SUMS"
}

# Ensure TLS 1.2
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
} catch { }

# Create install directory
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# Download
Write-Host "Downloading git-ai ($Version)..."
$TmpZip = Join-Path $InstallDir "git-ai-download.$PID.zip"

try {
    $oldProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $DownloadUrl -OutFile $TmpZip -UseBasicParsing -ErrorAction Stop
    } finally {
        $ProgressPreference = $oldProgress
    }
} catch {
    Remove-Item -Force -ErrorAction SilentlyContinue $TmpZip
    Write-ErrorAndExit "Failed to download from $DownloadUrl : $_"
}

if (-not (Test-Path $TmpZip) -or (Get-Item $TmpZip).Length -eq 0) {
    Remove-Item -Force -ErrorAction SilentlyContinue $TmpZip
    Write-ErrorAndExit "Downloaded file is empty or missing"
}

# Verify checksum
$SkipChecksum = $false
$ChecksumFile = Join-Path $InstallDir "SHA256SUMS.$PID"
try {
    $oldProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $ChecksumUrl -OutFile $ChecksumFile -UseBasicParsing -ErrorAction Stop
    } finally {
        $ProgressPreference = $oldProgress
    }
} catch {
    Write-Warn "Could not download checksums file, skipping verification"
    $SkipChecksum = $true
}

if (-not $SkipChecksum -and (Test-Path $ChecksumFile)) {
    $ExpectedLine = Get-Content $ChecksumFile | Where-Object { $_ -match $ZipName }
    if ($ExpectedLine) {
        $Expected = ($ExpectedLine -split '\s+')[0].ToLower()
        $Actual = (Get-FileHash -Path $TmpZip -Algorithm SHA256).Hash.ToLower()
        if ($Expected -ne $Actual) {
            Remove-Item -Force -ErrorAction SilentlyContinue $TmpZip
            Remove-Item -Force -ErrorAction SilentlyContinue $ChecksumFile
            Write-ErrorAndExit "Checksum mismatch for $ZipName`n  expected: $Expected`n  actual:   $Actual"
        }
        Write-Ok "Checksum verified."
    } else {
        Write-Warn "No checksum found for $ZipName in SHA256SUMS"
    }
    Remove-Item -Force -ErrorAction SilentlyContinue $ChecksumFile
}

# Extract
Write-Host "Extracting to $InstallDir..."
$ExtractDir = Join-Path $InstallDir "extract.$PID"
try {
    Expand-Archive -Path $TmpZip -DestinationPath $ExtractDir -Force

    # Move binary to install location
    $ExePath = Join-Path $ExtractDir 'git-ai.exe'
    if (-not (Test-Path $ExePath)) {
        # Try finding it in a subdirectory
        $ExePath = Get-ChildItem -Path $ExtractDir -Filter 'git-ai.exe' -Recurse | Select-Object -First 1 -ExpandProperty FullName
    }
    if (-not $ExePath -or -not (Test-Path $ExePath)) {
        Write-ErrorAndExit "git-ai.exe not found in archive"
    }

    $FinalExe = Join-Path $InstallDir 'git-ai.exe'
    Move-Item -Force -Path $ExePath -Destination $FinalExe
    try { Unblock-File -Path $FinalExe -ErrorAction SilentlyContinue } catch { }
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $ExtractDir
    Remove-Item -Force -ErrorAction SilentlyContinue $TmpZip
}

# Add to PATH
$CurrentUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$NormalizedInstall = $InstallDir.TrimEnd('\').ToLowerInvariant()
$AlreadyInPath = $false

if ($CurrentUserPath) {
    $entries = $CurrentUserPath -split ';' | Where-Object { $_.Trim() -ne '' }
    foreach ($entry in $entries) {
        if ($entry.Trim().TrimEnd('\').ToLowerInvariant() -eq $NormalizedInstall) {
            $AlreadyInPath = $true
            break
        }
    }
}

if (-not $AlreadyInPath) {
    try {
        $NewPath = "$InstallDir;$CurrentUserPath"
        [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
        Write-Ok "Added $InstallDir to user PATH."
    } catch {
        Write-Warn "Could not update PATH automatically. Please add '$InstallDir' to your PATH manually."
    }
}

# Update current session PATH
if ($env:PATH -notlike "*$InstallDir*") {
    $env:PATH = "$InstallDir;$env:PATH"
}

# Run git-ai install
Write-Host "Running git-ai install..."
$GitAiExe = Join-Path $InstallDir 'git-ai.exe'
try {
    & $GitAiExe install 2>$null
    if ($LASTEXITCODE -eq 0) {
        Write-Ok "git-ai install completed successfully."
    } else {
        Write-Warn "git-ai install exited with code $LASTEXITCODE. You may need to run 'git-ai install' manually."
    }
} catch {
    Write-Warn "Failed to run git-ai install: $_"
}

Write-Host ''
Write-Ok "git-ai has been installed to $InstallDir"
Write-Host ''

# Show version
try {
    $VersionOutput = & $GitAiExe --version 2>&1
    Write-Host "Installed: $VersionOutput"
} catch { }

Write-Host ''
Write-Host 'Restart your terminal to use git-ai.' -ForegroundColor Yellow
