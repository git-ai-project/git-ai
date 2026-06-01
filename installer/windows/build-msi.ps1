param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath,

    [Parameter(Mandatory = $false)]
    [string]$Version = '',

    [Parameter(Mandatory = $false)]
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64',

    [Parameter(Mandatory = $false)]
    [string]$OutputPath = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# --- Resolve version from Cargo.toml if not provided ---
if (-not $Version) {
    $cargoToml = Join-Path $PSScriptRoot '..\..\Cargo.toml'
    if (Test-Path $cargoToml) {
        $match = Select-String -Path $cargoToml -Pattern '^version = "(.+)"'
        if ($match) {
            $Version = $match.Matches[0].Groups[1].Value
        }
    }
    if (-not $Version) {
        Write-Error 'Could not determine version. Pass -Version explicitly or ensure Cargo.toml is accessible.'
        exit 1
    }
}

# --- Validate inputs ---
if (-not (Test-Path $BinaryPath)) {
    Write-Error "Binary not found: $BinaryPath"
    exit 1
}

# --- Paths ---
$scriptDir = $PSScriptRoot
$idtDir = Join-Path $scriptDir 'idt'
$buildDir = Join-Path $scriptDir 'build'
$stageDir = Join-Path $buildDir 'stage'

if (Test-Path $buildDir) { Remove-Item -Recurse -Force $buildDir -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

$archSuffix = if ($Architecture -eq 'arm64') { 'arm64' } else { 'x64' }
if (-not $OutputPath) {
    $OutputPath = Join-Path $buildDir "git-ai-$archSuffix.msi"
}
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)

Write-Host "Building MSI: version=$Version arch=$Architecture"
Write-Host "  Binary: $BinaryPath"
Write-Host "  Output: $OutputPath"

# --- Stage binaries ---
Copy-Item -Force $BinaryPath (Join-Path $stageDir 'git-ai.exe')
Copy-Item -Force $BinaryPath (Join-Path $stageDir 'git.exe')

# --- Generate deterministic ProductCode from version + arch ---
# RFC 4122 v5 UUID (SHA-1 namespace) seeded with UpgradeCode + version + arch
function New-DeterministicGuid {
    param([string]$Seed)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Seed)
    $hash = [System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)
    # Use first 16 bytes, set version 4 and variant bits for a valid GUID format
    $hash[6] = ($hash[6] -band 0x0F) -bor 0x40  # version 4
    $hash[8] = ($hash[8] -band 0x3F) -bor 0x80  # variant 1
    $guid = [System.Guid]::new(
        [System.BitConverter]::ToInt32($hash, 0),
        [System.BitConverter]::ToInt16($hash, 4),
        [System.BitConverter]::ToInt16($hash, 6),
        $hash[8], $hash[9], $hash[10], $hash[11],
        $hash[12], $hash[13], $hash[14], $hash[15]
    )
    return $guid.ToString('B').ToUpper()
}

$productCode = New-DeterministicGuid -Seed "git-ai-msi-$Version-$Architecture"
$packageCode = New-DeterministicGuid -Seed "git-ai-msi-pkg-$Version-$Architecture"
Write-Host "  ProductCode: $productCode"
Write-Host "  PackageCode: $packageCode"

# --- Prepare IDT files with version/GUID substitution ---
$idtBuildDir = Join-Path $buildDir 'idt'
New-Item -ItemType Directory -Force -Path $idtBuildDir | Out-Null

# Copy all IDT files, patching Property.idt with version-specific values
$idtFiles = Get-ChildItem -Path $idtDir -Filter '*.idt'
foreach ($file in $idtFiles) {
    if ($file.Name -eq 'Property.idt') {
        $content = Get-Content -Path $file.FullName -Raw
        # Append version and product code rows
        $content = $content.TrimEnd("`r", "`n") + "`r`n"
        $content += "ProductCode`t$productCode`r`n"
        $content += "ProductVersion`t$Version`r`n"
        Set-Content -Path (Join-Path $idtBuildDir $file.Name) -Value $content -NoNewline -Encoding ASCII
    } else {
        Copy-Item -Force $file.FullName (Join-Path $idtBuildDir $file.Name)
    }
}

# Patch File.idt with actual file sizes
$gitAiSize = (Get-Item (Join-Path $stageDir 'git-ai.exe')).Length
$gitShimSize = (Get-Item (Join-Path $stageDir 'git.exe')).Length
$fileIdt = Get-Content -Path (Join-Path $idtBuildDir 'File.idt') -Raw
$fileIdt = $fileIdt -replace "(GitAiExe`tGitAiComponent`tgit-ai\.exe`t)0", "`${1}$gitAiSize"
$fileIdt = $fileIdt -replace "(GitShimExe`tGitAiComponent`tgit\.exe`t)0", "`${1}$gitShimSize"
Set-Content -Path (Join-Path $idtBuildDir 'File.idt') -Value $fileIdt -NoNewline -Encoding ASCII

# --- Create cabinet (.cab) file ---
Write-Host 'Creating cabinet file...'
$ddfPath = Join-Path $buildDir 'git-ai.ddf'
$cabDir = Join-Path $buildDir 'cab'
New-Item -ItemType Directory -Force -Path $cabDir | Out-Null

$ddfContent = @"
.OPTION EXPLICIT
.Set CabinetNameTemplate=git-ai.cab
.Set DiskDirectoryTemplate="$cabDir"
.Set MaxDiskSize=0
.Set Cabinet=ON
.Set Compress=ON
.Set CompressionType=MSZIP
"$(Join-Path $stageDir 'git-ai.exe')" GitAiExe
"$(Join-Path $stageDir 'git.exe')" GitShimExe
"@
Set-Content -Path $ddfPath -Value $ddfContent -Encoding ASCII

makecab.exe /F $ddfPath
if ($LASTEXITCODE -ne 0) {
    Write-Error 'makecab.exe failed'
    exit 1
}

$cabPath = Join-Path $cabDir 'git-ai.cab'
if (-not (Test-Path $cabPath)) {
    Write-Error "Cabinet file not found at expected path: $cabPath"
    exit 1
}
Write-Host "  Cabinet: $cabPath ($('{0:N0}' -f (Get-Item $cabPath).Length) bytes)"

# --- Locate Windows SDK tools ---
function Find-SdkTool {
    param([string]$ToolName)

    # Check PATH first
    $cmd = Get-Command $ToolName -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    # Search Windows SDK directories
    $sdkRoots = @(
        "${env:ProgramFiles(x86)}\Windows Kits\10\bin",
        "$env:ProgramFiles\Windows Kits\10\bin"
    )
    foreach ($root in $sdkRoots) {
        if (-not (Test-Path $root)) { continue }
        $found = Get-ChildItem -Path $root -Recurse -Filter $ToolName -ErrorAction SilentlyContinue |
            Sort-Object { $_.Directory.Name } -Descending |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}

$msidb = Find-SdkTool 'msidb.exe'
if (-not $msidb) {
    Write-Error 'msidb.exe not found. Install the Windows SDK (Desktop App C++ Build Tools component).'
    exit 1
}
Write-Host "  msidb: $msidb"

# --- Build MSI database ---
Write-Host 'Importing IDT tables into MSI...'

# msidb is fragile with paths — always build in the IDT directory with a simple filename,
# then move the result to $OutputPath at the end.
$msiTempName = 'git-ai.msi'
$msiTempPath = Join-Path $idtBuildDir $msiTempName

# Delete stale MSI in both locations
if (Test-Path -LiteralPath $msiTempPath) { Remove-Item -LiteralPath $msiTempPath -Force }
if (Test-Path -LiteralPath $OutputPath) { Remove-Item -LiteralPath $OutputPath -Force }

# Read real table names and rename staged files to match what msidb expects
foreach ($file in $idtFiles) {
    $stagedFilePath = Join-Path $idtBuildDir $file.Name
    $thirdLine = (Get-Content -Path $stagedFilePath -TotalCount 3)[-1]
    $tableName = $thirdLine.Split("`t")[0]

    # Rename file to match internal table name (e.g. AdminExe.idt -> AdminExecuteSequence.idt)
    $expectedName = "$tableName.idt"
    if ($file.Name -ne $expectedName) {
        Rename-Item -LiteralPath $stagedFilePath -NewName $expectedName -Force
    }
}

Push-Location $idtBuildDir
try {
    # Start-Process with raw ArgumentList prevents PowerShell from mangling quotes/arrays.
    # The '*' tells msidb to import ALL .idt files in the folder.
    # Must use absolute path for -f (msidb rejects relative '.')
    $absoluteIdtPath = $idtBuildDir.TrimEnd('\')
    $argsStr = "-d `"$msiTempName`" -c -f `"$absoluteIdtPath`" -i *"
    Write-Host "  msidb: $argsStr"

    $proc = Start-Process -FilePath $msidb -ArgumentList $argsStr -Wait -NoNewWindow -PassThru
    if ($proc.ExitCode -ne 0) {
        Write-Error "msidb.exe import failed with exit code $($proc.ExitCode)"
        exit 1
    }

    if (-not (Test-Path -LiteralPath $msiTempName)) {
        Write-Error "msidb did not create MSI file. Check if the IDT files are formatted correctly."
        exit 1
    }

    # --- Inject cabinet ---
    Write-Host 'Injecting cabinet into MSI...'
    $cabArgs = "-d `"$msiTempName`" -a `"$cabPath`""
    $procCab = Start-Process -FilePath $msidb -ArgumentList $cabArgs -Wait -NoNewWindow -PassThru
    if ($procCab.ExitCode -ne 0) {
        Write-Error "msidb.exe cabinet injection failed with exit code $($procCab.ExitCode)"
        exit 1
    }
} finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $msiTempPath)) {
    Write-Error "MSI file was not generated at $msiTempPath"
    exit 1
}

# Move to final output location
$outputDir = [System.IO.Path]::GetDirectoryName($OutputPath)
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
Move-Item -Force -LiteralPath $msiTempPath -Destination $OutputPath

# Let antivirus/filesystem settle before opening via COM
Start-Sleep -Seconds 2

# --- Stamp Summary Information Stream via COM ---
# Uses the WindowsInstaller.Installer COM object (built into Windows) instead of
# msiinfo.exe which has unreliable flag semantics across SDK versions.
Write-Host 'Writing Summary Information Stream...'
$platform = if ($Architecture -eq 'arm64') { 'Arm64' } else { 'x64' }
$template = "$platform;1033"

$installer = New-Object -ComObject WindowsInstaller.Installer
# Suppress graphical popups — errors go to PowerShell instead
$installer.GetType().InvokeMember('UILevel', 'SetProperty', $null, $installer, @(2))
$database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($OutputPath, 1))
$summary = $database.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $database, @(20))

# PID_CODEPAGE (1) = 1252 (Windows Western)
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(1, 1252))
# PID_TITLE (2) = "Installation Database"
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(2, "Installation Database"))
# PID_SUBJECT (3)
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(3, "git-ai $Version"))
# PID_AUTHOR (4)
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(4, "git-ai-project"))
# PID_KEYWORDS (5)
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(5, "Installer,MSI,git-ai"))
# PID_COMMENTS (6)
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(6, "git-ai $Version ($Architecture)"))
# PID_TEMPLATE (7) - Platform;Language
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(7, $template))
# PID_REVNUMBER (9) - Package code GUID
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(9, $packageCode))
# PID_PAGECOUNT (14) - Minimum installer version (500 for Arm64, 200 for x64)
$minSchema = if ($Architecture -eq 'arm64') { 500 } else { 200 }
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(14, $minSchema))
# PID_WORDCOUNT (15) - Type flags: 2 = compressed source (cabinet embedded)
$summary.GetType().InvokeMember('Property', 'SetProperty', $null, $summary, @(15, 2))

$summary.GetType().InvokeMember('Persist', 'InvokeMethod', $null, $summary, $null)
$database.GetType().InvokeMember('Commit', 'InvokeMethod', $null, $database, $null)

[System.Runtime.Interopservices.Marshal]::ReleaseComObject($summary) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($database) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($installer) | Out-Null

# --- Done ---
$msiSize = (Get-Item $OutputPath).Length
Write-Host ''
Write-Host "MSI built successfully!" -ForegroundColor Green
Write-Host "  Path: $OutputPath"
Write-Host "  Size: $('{0:N0}' -f $msiSize) bytes"
Write-Host "  Install: msiexec /i `"$OutputPath`" /qn"
Write-Host "  Uninstall: msiexec /x $productCode /qn"
