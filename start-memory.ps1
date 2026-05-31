$ErrorActionPreference = "Stop"

$memoryDir = Join-Path $PSScriptRoot "memory"
$serverPath = Join-Path $memoryDir "node_modules\@tencentdb-agent-memory\memory-tencentdb\src\gateway\server.ts"

if (-not (Test-Path $serverPath)) {
    Write-Error "Server not found: $serverPath"
    exit 1
}

Set-Location $memoryDir
npx tsx $serverPath
