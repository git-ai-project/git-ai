$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$tempBase = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } elseif ($env:TEMP) { $env:TEMP } else { [System.IO.Path]::GetTempPath() }
$testRoot = Join-Path $tempBase ("git-ai-install-test-{0}" -f ([guid]::NewGuid().ToString()))

New-Item -ItemType Directory -Force -Path $testRoot | Out-Null

try {
    $homeDir = Join-Path $testRoot 'home'
    New-Item -ItemType Directory -Force -Path $homeDir | Out-Null
    $env:HOME = $homeDir
    $env:USERPROFILE = $homeDir
    $HOME = $homeDir

    $binDir = Join-Path $testRoot 'bin'
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    $claudeCmd = Join-Path $binDir 'claude.cmd'
    Set-Content -Path $claudeCmd -Value "@echo off`r`necho 2.0.0`r`n" -Encoding ASCII -Force
    $env:PATH = "$binDir;$env:PATH"

    $installScript = Join-Path $repoRoot 'install.ps1'
    $installOutput = & $installScript 2>&1 | Out-String

    $installDir = Join-Path $HOME '.git-ai\bin'
    $gitAiExe = Join-Path $installDir 'git-ai.exe'
    if (-not (Test-Path -LiteralPath $gitAiExe)) {
        throw "git-ai binary not found at $gitAiExe"
    }

    $versionOutput = & $gitAiExe --version | Out-String
    $versionMatch = [regex]::Match($versionOutput, '\d+\.\d+\.\d+[^\s]*')
    if (-not $versionMatch.Success) {
        throw "Unable to parse version from: $versionOutput"
    }
    $version = $versionMatch.Value

    $cmd = Get-Command git-ai -ErrorAction SilentlyContinue
    if (-not $cmd) {
        throw 'git-ai not available on PATH after install'
    }
    if ($cmd.Path -notlike "$installDir*") {
        throw "git-ai PATH points to unexpected location: $($cmd.Path)"
    }

    $settingsPath = Join-Path $HOME '.claude\settings.json'
    if (-not (Test-Path -LiteralPath $settingsPath)) {
        throw "Claude settings.json not created at $settingsPath"
    }
    $settingsContent = Get-Content -LiteralPath $settingsPath -Raw
    if ($settingsContent -notmatch [regex]::Escape('checkpoint claude --hook-input stdin')) {
        throw 'Claude hooks not configured in settings.json'
    }
    if ($settingsContent -notmatch [regex]::Escape($gitAiExe)) {
        throw 'git-ai path missing in Claude hooks config'
    }

    $overrideTag = "v$version"
    $env:GIT_AI_RELEASE_TAG = $overrideTag
    $overrideOutput = & $installScript 2>&1 | Out-String
    if ($overrideOutput -notmatch "release: $overrideTag") {
        throw "Override install did not report release tag $overrideTag"
    }
    Remove-Item Env:GIT_AI_RELEASE_TAG -ErrorAction SilentlyContinue

    & $gitAiExe --version | Out-Null
} finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
