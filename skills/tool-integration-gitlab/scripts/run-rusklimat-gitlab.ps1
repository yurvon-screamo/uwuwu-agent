$env:GITLAB_API_URL = if ($env:GITLAB_API_URL) { $env:GITLAB_API_URL } else { "https://gitlab.rusklimat.ru" }
if (-not $env:GITLAB_PERSONAL_ACCESS_TOKEN) {
    Write-Error "Error: GITLAB_PERSONAL_ACCESS_TOKEN not set. Set it via `$env:GITLAB_PERSONAL_ACCESS_TOKEN or system env."
    exit 1
}
$env:GITLAB_READ_ONLY_MODE = if ($env:GITLAB_READ_ONLY_MODE) { $env:GITLAB_READ_ONLY_MODE } else { "false" }
$env:USE_GITLAB_WIKI = if ($env:USE_GITLAB_WIKI) { $env:USE_GITLAB_WIKI } else { "false" }
$env:USE_MILESTONE = if ($env:USE_MILESTONE) { $env:USE_MILESTONE } else { "false" }
$env:USE_PIPELINE = if ($env:USE_PIPELINE) { $env:USE_PIPELINE } else { "true" }
bun "$PSScriptRoot\gitlab.ts" @args
