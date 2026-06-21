cd D:\uwuwu_agent\smos-rust

Stop-Process -Name smos -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

$free = (Get-PSDrive D).Free / 1GB
Write-Host "=== DISK FREE: $([Math]::Round($free, 1)) GB ==="

Write-Host "=== STARTING RELEASE BUILD ==="
$buildStartTime = Get-Date

$proc = Start-Process -FilePath "cargo" -ArgumentList "build","--release","--bin","smos","--features","smos-adapters/nli-directml" -NoNewWindow -PassThru -RedirectStandardOutput "build_out.txt" -RedirectStandardError "build_err.txt"
$proc | Wait-Process -Timeout 1800

$buildDuration = (Get-Date) - $buildStartTime

if (-not $proc.HasExited) {
    $proc | Stop-Process -Force
    Write-Host "=== BUILD TIMED OUT after 30 min ==="
    exit 1
}

Write-Host "=== BUILD EXIT: $($proc.ExitCode) ==="
Write-Host "=== BUILD DURATION: $($buildDuration.TotalMinutes.ToString('F1')) min ==="

if ($proc.ExitCode -ne 0) {
    Write-Host "=== BUILD ERRORS (last 30) ==="
    Get-Content build_err.txt -ErrorAction SilentlyContinue | Select-Object -Last 30
    Get-Content build_out.txt -ErrorAction SilentlyContinue | Select-Object -Last 30
    exit 1
}

Write-Host "=== BUILD STDOUT (last 5) ==="
Get-Content build_out.txt -ErrorAction SilentlyContinue | Select-Object -Last 5

$binPath = ".\target\release\smos.exe"
if (Test-Path $binPath) {
    $binSize = (Get-Item $binPath).Length / 1MB
    Write-Host "=== BINARY OK: $([Math]::Round($binSize, 1)) MB ==="
} else {
    Write-Host "=== BINARY NOT FOUND ==="
    exit 1
}

& $binPath --version
