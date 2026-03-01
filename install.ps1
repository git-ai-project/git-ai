$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Write-ErrorAndExit {
    param(
        [Parameter(Mandatory = $true)][string]$Message
    )
    Write-Host "Error: $Message" -ForegroundColor Red
    exit 1
}

function Write-Success {
    param(
        [Parameter(Mandatory = $true)][string]$Message
    )
    Write-Host $Message -ForegroundColor Green
}

function Write-Warning {
    param(
        [Parameter(Mandatory = $true)][string]$Message
    )
    Write-Host $Message -ForegroundColor Yellow
}

function Wait-ForFileAvailable {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $false)][int]$MaxWaitSeconds = 300,
        [Parameter(Mandatory = $false)][int]$RetryIntervalSeconds = 5
    )
    
    $elapsed = 0
    while ($elapsed -lt $MaxWaitSeconds) {
        try {
            # Try to open the file for writing to check if it's available
            $stream = [System.IO.File]::Open($Path, 'Open', 'Write', 'None')
            $stream.Close()
            return $true
        } catch {
            if ($elapsed -eq 0) {
                Write-Host "Waiting for file to be available: $Path" -ForegroundColor Yellow
            }
            Start-Sleep -Seconds $RetryIntervalSeconds
            $elapsed += $RetryIntervalSeconds
        }
    }
    return $false
}

function Verify-Checksum {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string]$BinaryName
    )

    # Skip verification if no checksums are embedded
    if ($EmbeddedChecksums -eq '__CHECKSUMS_PLACEHOLDER__') {
        return
    }

    # Extract expected checksum for this binary
    $expected = $null
    $entries = $EmbeddedChecksums -split '\|'
    foreach ($entry in $entries) {
        if ($entry -match "^([0-9a-fA-F]+)\s+$([regex]::Escape($BinaryName))$") {
            $expected = $Matches[1]
            break
        }
    }

    if (-not $expected) {
        Write-ErrorAndExit "No checksum found for $BinaryName"
    }

    # Calculate actual checksum
    $hashCommand = Get-Command Get-FileHash -ErrorAction SilentlyContinue
    if ($null -ne $hashCommand) {
        $actual = (Get-FileHash -Path $File -Algorithm SHA256).Hash.ToLower()
    } else {
        $stream = [System.IO.File]::OpenRead($File)
        try {
            $sha256 = [System.Security.Cryptography.SHA256]::Create()
            $hashBytes = $sha256.ComputeHash($stream)
            $actual = ([System.BitConverter]::ToString($hashBytes)).Replace('-', '').ToLower()
        } finally {
            $stream.Dispose()
            if ($sha256) {
                $sha256.Dispose()
            }
        }
    }

    if ($expected -ne $actual) {
        Remove-Item -Force -ErrorAction SilentlyContinue $File
        Write-ErrorAndExit "Checksum verification failed for $BinaryName`nExpected: $expected`nActual:   $actual"
    }

    Write-Success "Checksum verified for $BinaryName"
}

# GitHub repository details
# Replaced during release builds with the actual repository (e.g., "git-ai-project/git-ai")
# When set to __REPO_PLACEHOLDER__, defaults to "git-ai-project/git-ai"
$Repo = '__REPO_PLACEHOLDER__'
if ($Repo -eq '__REPO_PLACEHOLDER__') {
    $Repo = 'git-ai-project/git-ai'
}

# Version placeholder - replaced during release builds with actual version (e.g., "v1.0.24")
# When set to __VERSION_PLACEHOLDER__, defaults to "latest"
$PinnedVersion = '__VERSION_PLACEHOLDER__'

# Embedded checksums - replaced during release builds with actual SHA256 checksums
# Format: "hash  filename|hash  filename|..." (pipe-separated)
# When set to __CHECKSUMS_PLACEHOLDER__, checksum verification is skipped
$EmbeddedChecksums = '__CHECKSUMS_PLACEHOLDER__'

# Ensure TLS 1.2 for GitHub downloads on older PowerShell versions
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
} catch { }

function Get-Architecture {
    try {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
        switch ($arch) {
            'X64' { return 'x64' }
            'Arm64' { return 'arm64' }
            default { return $null }
        }
    } catch {
        $pa = $env:PROCESSOR_ARCHITECTURE
        if ($pa -match 'ARM64') { return 'arm64' }
        elseif ($pa -match '64') { return 'x64' }
        else { return $null }
    }
}

function Get-StdGitPath {
    $cmd = Get-Command git.exe -ErrorAction SilentlyContinue
    $gitPath = $null
    if ($cmd -and $cmd.Path) {
        # Ensure we never return a path for git that contains git-ai (recursive)
        if ($cmd.Path -notmatch "git-ai") {
            $gitPath = $cmd.Path
        }
    }

    # If detection failed or was our own shim, try to recover from saved config
    if (-not $gitPath) {
        try {
            $cfgPath = Join-Path $HOME ".git-ai\config.json"
            if (Test-Path -LiteralPath $cfgPath) {
                $cfg = Get-Content -LiteralPath $cfgPath -Raw | ConvertFrom-Json
                if ($cfg -and $cfg.git_path -and ($cfg.git_path -notmatch 'git-ai') -and (Test-Path -LiteralPath $cfg.git_path)) {
                    $gitPath = $cfg.git_path
                }
            }
        } catch { }
    }

    # If still not found, fail with a clear message
    if (-not $gitPath) {
        Write-ErrorAndExit "Could not detect a standard git binary on PATH. Please ensure you have Git installed and available on your PATH. If you believe this is a bug with the installer, please file an issue at https://github.com/git-ai-project/git-ai/issues."
    }

    try {
        & $gitPath --version | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'bad' }
    } catch {
        Write-ErrorAndExit "Detected git at $gitPath is not usable (--version failed). Please ensure you have Git installed and available on your PATH. If you believe this is a bug with the installer, please file an issue at https://github.com/git-ai-project/git-ai/issues."
    }

    return $gitPath
}

# Ensure $PathToAdd is inserted before any PATH entry that contains "git" (case-insensitive)
# Updates Machine (system) PATH; if not elevated, emits a prominent error with instructions
function Set-PathPrependBeforeGit {
    param(
        [Parameter(Mandatory = $true)][string]$PathToAdd
    )

    $sep = ';'

    function NormalizePath([string]$p) {
        try { return ([IO.Path]::GetFullPath($p.Trim())).TrimEnd('\\').ToLowerInvariant() }
        catch { return ($p.Trim()).TrimEnd('\\').ToLowerInvariant() }
    }

    $normalizedAdd = NormalizePath $PathToAdd

    # Helper to build new PATH string with PathToAdd inserted before first 'git' entry
    function BuildPathWithInsert([string]$existingPath, [string]$toInsert) {
        $entries = @()
        if ($existingPath) { $entries = ($existingPath -split $sep) | Where-Object { $_ -and $_.Trim() -ne '' } }

        # De-duplicate and remove any existing instance of $toInsert
        $list = New-Object System.Collections.Generic.List[string]
        $seen = New-Object 'System.Collections.Generic.HashSet[string]'
        foreach ($e in $entries) {
            $n = NormalizePath $e
            if (-not $seen.Contains($n) -and $n -ne $normalizedAdd) {
                $seen.Add($n) | Out-Null
                $list.Add($e) | Out-Null
            }
        }

        # Find first index that matches 'git' anywhere (case-insensitive)
        $insertIndex = 0
        for ($i = 0; $i -lt $list.Count; $i++) {
            if ($list[$i] -match '(?i)git') { $insertIndex = $i; break }
        }

        $list.Insert($insertIndex, $toInsert)
        return ($list -join $sep)
    }

    $userStatus = 'Skipped'
    try {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $newUserPath = BuildPathWithInsert -existingPath $userPath -toInsert $PathToAdd
        if ($newUserPath -ne $userPath) {
            [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
            $userStatus = 'Updated'
        } else {
            $userStatus = 'AlreadyPresent'
        }
    } catch {
        $userStatus = 'Error'
    }

    # Try to update Machine PATH
    $machineStatus = 'Skipped'
    try {
        $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
        $newMachinePath = BuildPathWithInsert -existingPath $machinePath -toInsert $PathToAdd
        if ($newMachinePath -ne $machinePath) {
            [Environment]::SetEnvironmentVariable('Path', $newMachinePath, 'Machine')
            $machineStatus = 'Updated'
        } else {
            # Nothing changed at Machine scope; still treat as Machine for reporting
            $machineStatus = 'AlreadyPresent'
        }
    } catch {
        # Access denied or not elevated; do NOT modify User PATH. Print big red error with instructions.
        $origGit = $null
        try { $origGit = Get-StdGitPath } catch { }
        $origGitDir = if ($origGit) { (Split-Path $origGit -Parent) } else { 'your Git installation directory' }
        Write-Host ''
        Write-Host 'ERROR: Unable to update the SYSTEM PATH (administrator rights required).' -ForegroundColor Red
        Write-Host 'Your PATH was NOT changed. To ensure git-ai takes precedence over Git:' -ForegroundColor Red
        Write-Host ("  1) Run PowerShell as Administrator and re-run this installer; OR") -ForegroundColor Red
        Write-Host ("  2) Manually edit the SYSTEM Path and move '{0}' before any entries containing 'Git' (e.g. '{1}')." -f $PathToAdd, $origGitDir) -ForegroundColor Red
        Write-Host "     Steps: Start → type 'Environment Variables' → 'Edit the system environment variables' → Environment Variables →" -ForegroundColor Red
        Write-Host "            Under 'System variables', select 'Path' → Edit → Move '{0}' to the top (before Git) → OK." -f $PathToAdd -ForegroundColor Red
        Write-Host ''
        if ($userStatus -eq 'Updated' -or $userStatus -eq 'AlreadyPresent') {
            Write-Host 'User PATH was updated successfully, so git-ai will still take precedence for this account.' -ForegroundColor Yellow
        }
        $machineStatus = 'Error'
    }

    # Update current process PATH immediately for this session
    try {
        $procPath = $env:PATH
        $newProcPath = BuildPathWithInsert -existingPath $procPath -toInsert $PathToAdd
        if ($newProcPath -ne $procPath) { $env:PATH = $newProcPath }
    } catch { }

    return [PSCustomObject]@{
        UserStatus    = $userStatus
        MachineStatus = $machineStatus
    }
}

# Detect standard Git early and validate (fail-fast behavior)
$stdGitPath = Get-StdGitPath

# Detect architecture and OS
$arch = Get-Architecture
if (-not $arch) { Write-ErrorAndExit "Unsupported architecture: $([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture)" }
$os = 'windows'

# Determine binary name and download URLs
$binaryName = "git-ai-$os-$arch"

# Determine release tag
# Priority: 1. Local binary override, 2. Pinned version (for release builds), 3. Environment variable, 4. "latest"
if (-not [string]::IsNullOrWhiteSpace($env:GIT_AI_LOCAL_BINARY)) {
    $releaseTag = 'local'
} elseif ($PinnedVersion -ne '__VERSION_PLACEHOLDER__') {
    # Version-pinned install script from a release
    $releaseTag = $PinnedVersion
    $downloadUrlExe = "https://usegitai.com/worker/releases/download/$releaseTag/$binaryName.exe"
    $downloadUrlNoExt = "https://usegitai.com/worker/releases/download/$releaseTag/$binaryName"
} elseif (-not [string]::IsNullOrWhiteSpace($env:GIT_AI_RELEASE_TAG) -and $env:GIT_AI_RELEASE_TAG -ne 'latest') {
    # Environment variable override
    $releaseTag = $env:GIT_AI_RELEASE_TAG
    $downloadUrlExe = "https://usegitai.com/worker/releases/download/$releaseTag/$binaryName.exe"
    $downloadUrlNoExt = "https://usegitai.com/worker/releases/download/$releaseTag/$binaryName"
} else {
    # Default to latest
    $releaseTag = 'latest'
    $downloadUrlExe = "https://usegitai.com/worker/releases/download/latest/$binaryName.exe"
    $downloadUrlNoExt = "https://usegitai.com/worker/releases/download/latest/$binaryName"
}

# Install directory: %USERPROFILE%\.git-ai\bin
$installDir = Join-Path $HOME ".git-ai\bin"
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

Write-Host ("Downloading git-ai (release: {0})..." -f $releaseTag)
$tmpFile = Join-Path $installDir "git-ai.tmp.$PID.exe"

function Try-Download {
    param(
        [Parameter(Mandatory = $true)][string]$Url
    )
    try {
        Invoke-WebRequest -Uri $Url -OutFile $tmpFile -UseBasicParsing -ErrorAction Stop
        return $true
    } catch {
        return $false
    }
}

# Track which download URL succeeded for checksum verification
$downloadedBinaryName = $null
if (-not [string]::IsNullOrWhiteSpace($env:GIT_AI_LOCAL_BINARY)) {
    if (-not (Test-Path -LiteralPath $env:GIT_AI_LOCAL_BINARY)) {
        Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
        Write-ErrorAndExit "Local binary not found at $($env:GIT_AI_LOCAL_BINARY)"
    }
    Copy-Item -Force -Path $env:GIT_AI_LOCAL_BINARY -Destination $tmpFile
    $downloadedBinaryName = "$binaryName.exe"
} elseif (Try-Download -Url $downloadUrlExe) {
    $downloadedBinaryName = "$binaryName.exe"
} elseif (Try-Download -Url $downloadUrlNoExt) {
    $downloadedBinaryName = $binaryName
}

if (-not $downloadedBinaryName) {
    Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
    Write-ErrorAndExit 'Failed to download binary (HTTP error)'
}

try {
    if ((Get-Item $tmpFile).Length -le 0) {
        Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
        Write-ErrorAndExit 'Downloaded file is empty'
    }
} catch {
    Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
    Write-ErrorAndExit 'Download failed'
}

# Verify checksum if embedded (release builds only)
Verify-Checksum -File $tmpFile -BinaryName $downloadedBinaryName

$finalExe = Join-Path $installDir 'git-ai.exe'

# Wait for git-ai.exe to be available if it exists and is in use
if (Test-Path -LiteralPath $finalExe) {
    if (-not (Wait-ForFileAvailable -Path $finalExe -MaxWaitSeconds 300 -RetryIntervalSeconds 5)) {
        Remove-Item -Force -ErrorAction SilentlyContinue $tmpFile
        Write-ErrorAndExit "Timeout waiting for $finalExe to be available. Please close any running git-ai processes and try again."
    }
}

Move-Item -Force -Path $tmpFile -Destination $finalExe
try { Unblock-File -Path $finalExe -ErrorAction SilentlyContinue } catch { }

# Create a shim so calling `git` goes through git-ai by PATH precedence
$gitShim = Join-Path $installDir 'git.exe'

# Wait for git.exe shim to be available if it exists and is in use
if (Test-Path -LiteralPath $gitShim) {
    if (-not (Wait-ForFileAvailable -Path $gitShim -MaxWaitSeconds 300 -RetryIntervalSeconds 5)) {
        Write-ErrorAndExit "Timeout waiting for $gitShim to be available. Please close any running git processes and try again."
    }
}

Copy-Item -Force -Path $finalExe -Destination $gitShim
try { Unblock-File -Path $gitShim -ErrorAction SilentlyContinue } catch { }

# Create a shim so calling `git-og` invokes the standard Git
$gitOgShim = Join-Path $installDir 'git-og.cmd'
$gitOgShimContent = "@echo off$([Environment]::NewLine)`"$stdGitPath`" %*$([Environment]::NewLine)"
Set-Content -Path $gitOgShim -Value $gitOgShimContent -Encoding ASCII -Force
try { Unblock-File -Path $gitOgShim -ErrorAction SilentlyContinue } catch { }

# Login user with install token if provided
$needLogin = $false
if ($env:INSTALL_NONCE -and $env:API_BASE) {
    try {
        & $finalExe exchange-nonce | Out-Host
        if ($LASTEXITCODE -ne 0) {
            $needLogin = $true
        }
    } catch {
        $needLogin = $true
    }
}

# Install hooks
Write-Host 'Setting up IDE/agent hooks...'
try {
    & $finalExe install-hooks | Out-Host
    Write-Success 'Successfully set up IDE/agent hooks'
} catch {
    Write-Warning "Warning: Failed to set up IDE/agent hooks. Please try running 'git-ai install-hooks' manually."
}

# Update PATH so our shim takes precedence over any Git entries
$pathUpdate = Set-PathPrependBeforeGit -PathToAdd $installDir
if ($pathUpdate.UserStatus -eq 'Updated') {
    Write-Success 'Successfully added git-ai to the user PATH.'
} elseif ($pathUpdate.UserStatus -eq 'AlreadyPresent') {
    Write-Success 'git-ai already present in the user PATH.'
} elseif ($pathUpdate.UserStatus -eq 'Error') {
    Write-Host 'Failed to update the user PATH.' -ForegroundColor Red
}

if ($pathUpdate.MachineStatus -eq 'Updated') {
    Write-Success 'Successfully added git-ai to the system PATH.'
} elseif ($pathUpdate.MachineStatus -eq 'AlreadyPresent') {
    Write-Success 'git-ai already present in the system PATH.'
} elseif ($pathUpdate.MachineStatus -eq 'Error') {
    Write-Host 'PATH update failed: system PATH unchanged.' -ForegroundColor Red
}

Write-Success "Successfully installed git-ai into $installDir"
Write-Success "You can now run 'git-ai' from your terminal"

# Configure Git Bash shell profiles so git-ai takes precedence over /mingw64/bin/git
# Git Bash (MSYS2/MinGW) prepends its own directories to PATH, which shadows
# the Windows PATH entry we set above. Writing to ~/.bashrc ensures git-ai's
# bin directory is prepended after Git Bash's own PATH setup.
$gitBashConfigured = $false
$gitBashAlreadyConfigured = $false
try {
    $bashrcPath = Join-Path $HOME '.bashrc'
    $bashProfilePath = Join-Path $HOME '.bash_profile'
    $pathCmd = 'export PATH="$HOME/.git-ai/bin:$PATH"'
    $markerString = '.git-ai/bin'

    # Detect if Git Bash is installed
    $gitBashInstalled = $false
    $gitForWindowsPaths = @()
    if ($env:ProgramFiles) { $gitForWindowsPaths += Join-Path $env:ProgramFiles 'Git\bin\bash.exe' }
    if (${env:ProgramFiles(x86)}) { $gitForWindowsPaths += Join-Path ${env:ProgramFiles(x86)} 'Git\bin\bash.exe' }
    if ($env:LOCALAPPDATA) { $gitForWindowsPaths += Join-Path $env:LOCALAPPDATA 'Programs\Git\bin\bash.exe' }
    foreach ($p in $gitForWindowsPaths) {
        if ($p -and (Test-Path -LiteralPath $p)) {
            $gitBashInstalled = $true
            break
        }
    }

    if ($gitBashInstalled) {
        # Determine which config file to update (prefer .bashrc, fall back to .bash_profile)
        $targetBashConfig = $null
        if (Test-Path -LiteralPath $bashrcPath) {
            $targetBashConfig = $bashrcPath
        } elseif (Test-Path -LiteralPath $bashProfilePath) {
            $targetBashConfig = $bashProfilePath
        } else {
            # No existing config; create .bashrc
            $targetBashConfig = $bashrcPath
        }

        # Check if already configured
        $alreadyPresent = $false
        if (Test-Path -LiteralPath $targetBashConfig) {
            $content = Get-Content -LiteralPath $targetBashConfig -Raw -ErrorAction SilentlyContinue
            if ($content -and $content.Contains($markerString)) {
                $alreadyPresent = $true
            }
        }

        if ($alreadyPresent) {
            $gitBashAlreadyConfigured = $true
        } else {
            $timestamp = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'
            $appendContent = "`n# Added by git-ai installer on $timestamp`n$pathCmd`n"
            $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
            [System.IO.File]::AppendAllText($targetBashConfig, $appendContent, $utf8NoBom)
            $gitBashConfigured = $true
        }
    }
} catch {
    Write-Host "Warning: Failed to configure Git Bash: $($_.Exception.Message)" -ForegroundColor Yellow
}

if ($gitBashConfigured) {
    Write-Success "Successfully configured Git Bash ($targetBashConfig)"
} elseif ($gitBashAlreadyConfigured) {
    Write-Success "Git Bash already configured ($targetBashConfig)"
}

# Write JSON config at %USERPROFILE%\.git-ai\config.json (only if it doesn't exist)
try {
    $configDir = Join-Path $HOME '.git-ai'
    $configJsonPath = Join-Path $configDir 'config.json'
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null

    if (-not (Test-Path -LiteralPath $configJsonPath)) {
        $cfg = @{
            git_path = $stdGitPath
        } | ConvertTo-Json -Compress
        $cfg | Out-File -FilePath $configJsonPath -Encoding UTF8 -Force
    }
} catch {
    Write-Host "Warning: Failed to write config.json: $($_.Exception.Message)" -ForegroundColor Yellow
}

Write-Host 'Close and reopen your terminal and IDE sessions to use git-ai.' -ForegroundColor Yellow

# If nonce exchange failed, run interactive login
if ($needLogin) {
    Write-Host ''
    Write-Host 'Launching login...'
    & $finalExe login
}
