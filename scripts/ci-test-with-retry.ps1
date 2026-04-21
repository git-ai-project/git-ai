# Retry logic for flaky tests in daemon and wrapper-daemon modes (Windows).
# Only re-runs failed tests (not the full suite) for speed.
# Exits 0 with a warning if flaky tests pass on retry.

param(
    [int]$TestThreads = 4
)

$ErrorActionPreference = "Continue"
$TestMode = $env:GIT_AI_TEST_GIT_MODE

# Run the full test suite, streaming output to console and capturing to a temp file
$tempFile = [System.IO.Path]::GetTempFileName()
& cargo test -- --test-threads=$TestThreads 2>&1 | Tee-Object -FilePath $tempFile
$firstExit = $LASTEXITCODE

if ($firstExit -eq 0) {
    Remove-Item -Path $tempFile -Force -ErrorAction SilentlyContinue
    exit 0
}

# Only retry for daemon and wrapper-daemon modes
if ($TestMode -ne "daemon" -and $TestMode -ne "wrapper-daemon") {
    Write-Host "::error::Tests failed in '$TestMode' mode (retry not enabled for this mode)"
    Remove-Item -Path $tempFile -Force -ErrorAction SilentlyContinue
    exit 1
}

# Parse failed test names from cargo test output
$lines = Get-Content -Path $tempFile
Remove-Item -Path $tempFile -Force
$inFailures = $false
$failedTests = @()

foreach ($line in $lines) {
    $line = $line.TrimEnd()
    if ($line -eq "failures:") {
        $inFailures = $true
        continue
    }
    if ($inFailures -and ($line -eq "" -or $line -match "^test result:")) {
        $inFailures = $false
        continue
    }
    if ($inFailures -and $line -match "^\s+(\S+)") {
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

$failedCount = $failedTests.Count

if ($failedCount -gt 5) {
    Write-Host "::error::$failedCount tests failed on first run — too many failures to retry as flaky"
    exit 1
}

Write-Host ""
Write-Host "::warning::$failedCount test(s) failed on first run in '$TestMode' mode. Retrying individually..."
Write-Host ""

# Retry each failed test individually
$stillFailing = @()
$passedOnRetry = @()

foreach ($testName in $failedTests) {
    Write-Host "--- Retrying: $testName ---"
    cargo test $testName -- --test-threads=1 --exact
    if ($LASTEXITCODE -eq 0) {
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

Write-Host "::warning::All $failedCount previously-failed test(s) passed on retry (flaky in '$TestMode' mode):"
foreach ($t in $passedOnRetry) {
    Write-Host "  - $t"
}
exit 0
