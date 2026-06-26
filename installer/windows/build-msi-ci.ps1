# CI wrapper that builds MSIs for all Windows architectures.
# Expects release binaries to already exist in the artifacts directory.
#
# Usage:
#   .\build-msi-ci.ps1 -ArtifactsDir "path\to\artifacts" [-Version "1.4.6"]

param(
    [Parameter(Mandatory = $true)]
    [string]$ArtifactsDir,

    [Parameter(Mandatory = $false)]
    [string]$Version = '',

    [Parameter(Mandatory = $false)]
    [string]$OutputDir = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$scriptDir = $PSScriptRoot
$buildScript = Join-Path $scriptDir 'build-msi.ps1'

if (-not $OutputDir) {
    $OutputDir = Join-Path $scriptDir 'build'
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$targets = @(
    @{ Arch = 'x64';   Binary = 'git-ai-windows-x64.exe' },
    @{ Arch = 'arm64'; Binary = 'git-ai-windows-arm64.exe' }
)

$built = @()

foreach ($target in $targets) {
    $binaryPath = Join-Path $ArtifactsDir $target.Binary
    if (-not (Test-Path $binaryPath)) {
        Write-Host "Skipping $($target.Arch): binary not found at $binaryPath" -ForegroundColor Yellow
        continue
    }

    $outputMsi = Join-Path $OutputDir "git-ai-$($target.Arch).msi"
    Write-Host "Building MSI for $($target.Arch)..." -ForegroundColor Cyan

    $params = @{
        BinaryPath   = $binaryPath
        Architecture = $target.Arch
        OutputPath   = $outputMsi
    }
    if ($Version) { $params.Version = $Version }

    & $buildScript @params
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Failed to build MSI for $($target.Arch)"
        exit 1
    }

    $built += $outputMsi
}

if ($built.Count -eq 0) {
    Write-Error 'No MSIs were built. Check that artifact binaries exist.'
    exit 1
}

Write-Host ''
Write-Host "Built $($built.Count) MSI(s):" -ForegroundColor Green
foreach ($msi in $built) {
    Write-Host "  $msi ($('{0:N0}' -f (Get-Item $msi).Length) bytes)"
}
