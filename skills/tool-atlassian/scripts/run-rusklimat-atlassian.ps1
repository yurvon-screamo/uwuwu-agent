if (-not $env:CONFLUENCE_PERSONAL_TOKEN) {
    Write-Error "Error: CONFLUENCE_PERSONAL_TOKEN not set. Set it via `$env:CONFLUENCE_PERSONAL_TOKEN or system env."
    exit 1
}
$env:CONFLUENCE_USERNAME = "turbin_y@rusklimat.ru"
if (-not $env:JIRA_PERSONAL_TOKEN) {
    Write-Error "Error: JIRA_PERSONAL_TOKEN not set. Set it via `$env:JIRA_PERSONAL_TOKEN or system env."
    exit 1
}
$env:JIRA_USERNAME = "turbin_y@rusklimat.ru"
$env:CONFLUENCE_URL = if ($env:CONFLUENCE_URL) { $env:CONFLUENCE_URL } else { "https://wiki.rusklimat.ru" }
$env:JIRA_URL = if ($env:JIRA_URL) { $env:JIRA_URL } else { "https://jira.rusklimat.ru/" }
bun "$PSScriptRoot\atlassian.ts" @args
