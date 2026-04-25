# Retry logic for flaky tests in daemon and wrapper-daemon modes (Windows).
# Only re-runs failed tests (not the full suite) for speed.
# Exits 0 with a warning if flaky tests pass on retry.

param(
    [int]$TestThreads = 4,
    [int]$RetryTimeoutSeconds = 600,
    [int]$FullRunTimeoutSeconds = 14400
)

$ErrorActionPreference = "Stop"
$TestMode = $env:GIT_AI_TEST_GIT_MODE

if ($IsWindows -or $env:OS -eq "Windows_NT") {
    $gitUsrBin = "C:\Program Files\Git\usr\bin"
    if ((Test-Path $gitUsrBin) -and -not (($env:Path -split ";") -contains $gitUsrBin)) {
        $env:Path = "$gitUsrBin;$env:Path"
    }
}

function ConvertTo-CmdArgument {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Argument
    )

    if ($Argument -match '^[A-Za-z0-9_./:=+\-]+$') {
        return $Argument
    }

    return '"' + ($Argument -replace '"', '\"') + '"'
}

function ConvertTo-CmdPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    return '"' + ($Path -replace '"', '\"') + '"'
}

function Invoke-CargoCaptured {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,
        [Parameter(Mandatory = $true)]
        [int]$TimeoutSeconds,
        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $stdoutFile = [System.IO.Path]::GetTempFileName()
    $stderrFile = [System.IO.Path]::GetTempFileName()

    try {
        $cargoCommand = "cargo " + (($Arguments | ForEach-Object { ConvertTo-CmdArgument $_ }) -join " ")
        $command = "{0} > {1} 2> {2}" -f $cargoCommand, (ConvertTo-CmdPath $stdoutFile), (ConvertTo-CmdPath $stderrFile)
        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = "cmd.exe"
        $startInfo.Arguments = "/S /C $command"
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $process = [System.Diagnostics.Process]::Start($startInfo)

        $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
        $nextProgress = (Get-Date).AddSeconds(60)
        while (-not $process.HasExited) {
            if ((Get-Date) -ge $deadline) {
                Write-Host "::error::${Label} timed out after ${TimeoutSeconds}s"
                & taskkill /F /T /PID $process.Id 2>$null | Out-Null
                try {
                    Wait-Process -Id $process.Id -Timeout 10 -ErrorAction SilentlyContinue
                } catch {
                }
                break
            }

            if ((Get-Date) -ge $nextProgress) {
                Write-Host "::notice::${Label} still running..."
                $nextProgress = (Get-Date).AddSeconds(60)
            }

            Start-Sleep -Seconds 1
            $process.Refresh()
        }

        if ($process.HasExited) {
            $process.WaitForExit()
        }

        $stdoutLines = if (Test-Path $stdoutFile) {
            @([System.IO.File]::ReadAllLines($stdoutFile))
        } else {
            @()
        }
        $stderrLines = if (Test-Path $stderrFile) {
            @([System.IO.File]::ReadAllLines($stderrFile))
        } else {
            @()
        }

        foreach ($line in $stdoutLines) {
            [Console]::Out.WriteLine($line)
        }
        foreach ($line in $stderrLines) {
            [Console]::Error.WriteLine($line)
        }

        $exitCode = if ($process.HasExited) { $process.ExitCode } else { 124 }
        [pscustomobject]@{
            ExitCode = $exitCode
            Lines = @($stdoutLines + $stderrLines)
        }
    } finally {
        Remove-Item -Path $stdoutFile -Force -ErrorAction SilentlyContinue
        Remove-Item -Path $stderrFile -Force -ErrorAction SilentlyContinue
    }
}

$fullRun = Invoke-CargoCaptured `
    -Arguments @("test", "--", "--test-threads=$TestThreads") `
    -TimeoutSeconds $FullRunTimeoutSeconds `
    -Label "cargo test"

if ($fullRun.ExitCode -eq 0) {
    exit 0
}

if ($fullRun.ExitCode -eq 124) {
    exit 1
}

# Parse failed test names from the cargo test failures section.
$inFailures = $false
$failedTests = @()

foreach ($line in $fullRun.Lines) {
    $trimmed = $line.TrimEnd()
    if ($trimmed -eq "failures:") {
        $inFailures = $true
        continue
    }
    if ($inFailures -and ($trimmed -eq "" -or $trimmed -match "^test result:")) {
        $inFailures = $false
        continue
    }
    if ($inFailures -and $trimmed -match "^\s+(\S+)") {
        $testName = $Matches[1].Trim()
        if ($testName -and $testName -ne "----") {
            $failedTests += $testName
        }
    }
}

if ($failedTests.Count -eq 0) {
    Write-Host "::error::Tests failed but could not parse failed test names for retry"
    exit 1
}

$failedTests = @($failedTests | Sort-Object -Unique)
$failedCount = $failedTests.Count

if ($failedCount -gt 5) {
    Write-Host ("::error::{0} tests failed on first run - too many failures to retry as flaky" -f $failedCount)
    exit 1
}

Write-Host ""
Write-Host ("::warning::{0} test(s) failed on first run in '{1}' mode. Retrying individually..." -f $failedCount, $TestMode)
Write-Host ""

$stillFailing = @()
$passedOnRetry = @()

foreach ($testName in $failedTests) {
    Write-Host "--- Retrying: $testName ---"
    $retryRun = Invoke-CargoCaptured `
        -Arguments @("test", $testName, "--", "--test-threads=1", "--exact") `
        -TimeoutSeconds $RetryTimeoutSeconds `
        -Label "retry $testName"

    if ($retryRun.ExitCode -eq 0) {
        $passedOnRetry += $testName
    } else {
        $stillFailing += $testName
    }
}

Write-Host ""

if ($stillFailing.Count -gt 0) {
    Write-Host "::error::The following tests failed even on retry:"
    foreach ($t in $stillFailing) {
        Write-Host "  - $t"
    }
    exit 1
}

Write-Host ("::warning::All {0} previously-failed test(s) passed on retry (flaky in '{1}' mode):" -f $failedCount, $TestMode)
foreach ($t in $passedOnRetry) {
    Write-Host "  - $t"
}
exit 0
