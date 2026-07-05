# Browser Mode Setup & Troubleshooting

Setting up Chrome extension, session issues, and common failures.

## Setup

1. Install autocli: `curl -fsSL https://raw.githubusercontent.com/nashsu/AutoCLI/main/scripts/install.sh | sh`
2. Install the autocli Chrome extension (from the repo releases)
3. Open Chrome and log into the target site(s)

## Diagnostics

```bash
autocli doctor
```

Checks:
- autocli binary present and version
- Chrome running and accessible
- Extension installed and connected
- Session validity for target sites

## Common Failures

| Symptom | Cause | Fix |
|---------|-------|-----|
| "browser not connected" | Chrome closed or extension not loaded | Open Chrome, ensure extension is active |
| "session expired" | Login session timed out | Log into the site manually in Chrome |
| "extension not found" | Extension not installed | Install from repo releases |
| Command hangs | Chrome page loading slowly | Wait or check network; `Ctrl+C` and retry |
| Empty output | Selector changed on site | Create/update YAML adapter |

## Modes Requiring Chrome

| Mode | Chrome Required? | Extension Required? |
|------|-------------------|----------------------|
| Public | No | No |
| Public/Browser | Yes (open) | No |
| Browser | Yes (open + logged in) | Yes |
| Desktop | No (app running) | No |
