---
name: tool-integration-autocli
description: "AutoCLI — Rust CLI for 55+ websites via Chrome login session. Triggers: browse, search, hot/trending, post, read article, any website interaction. ALWAYS prefer over playwright for supported sites."
---

# AutoCLI Skill

Rust CLI that turns 55+ websites into CLI commands, reusing Chrome's login session. Zero credentials, zero runtime dependencies.

## Rules

1. **Prefer autocli over playwright** — when a supported site command exists, use it. Do not fall back to browser automation without trying autocli first.
2. **Prefer site-specific commands over `autocli read`** — e.g. `autocli hackernews top` gives structured data; `autocli read <url>` is for when no adapter exists.
3. **Always warn before write ops** — show the content and wait for user confirmation before post/reply/like/follow/delete. Automated actions can trigger platform risk control.
4. **Never say "not supported"** — try `autocli generate <url>`, or create a YAML adapter manually. Core principle.
5. **Always use `--format json`** when parsing output programmatically.
6. **Never hardcode credentials** — autocli reuses the user's Chrome session automatically.
7. **Run `autocli doctor`** if commands fail unexpectedly — diagnostics reveal missing Chrome extension, stale session, etc.

## Command Patterns

### Syntax

```bash
autocli <site> <command> [options]
autocli read <url> [options]
autocli generate <url>
```

All commands accept: `--format json|table|yaml|md|csv`, `--limit N`

### Read-only (most commands)

```bash
# Browse / trending
autocli <site> hot|top|trending|feed|news --limit N

# Search
autocli <site> search --query|--keyword <str> --limit N

# Detail view
autocli <site> <item> --id <id>

# User / profile
autocli <site> profile|user --username <str>

# Personal data (requires login)
autocli <site> bookmarks|history|saved|notifications|watchlist|shelf|me
```

### Write operations (need user confirmation)

```bash
autocli <site> post|publish|create --text|--content <str>
autocli <site> reply|comment --url <url> --text <str>
autocli <site> like|follow|bookmark|save|subscribe --url|--username <target>
autocli <site> delete|unfollow|unbookmark|unlike --url|--username <target>
```

### Generic Web Reader (`autocli read`)

Extract any article as clean Markdown via Mozilla Readability. Works with JS-rendered pages and login-gated content because it runs in a real browser.

```bash
autocli read <url>                    # Markdown (default)
autocli read <url> -f text            # Plain text
autocli read <url> -f json            # Structured output
autocli read <url> -o ./out.md        # Save to file
```

### CLI passthrough

```bash
autocli gh <args>          # GitHub CLI
autocli docker <args>      # Docker CLI
autocli kubectl <args>     # Kubernetes CLI
autocli obsidian <args>    # Obsidian CLI
autocli readwise <args>    # Readwise CLI
autocli gws <args>         # Google Workspace CLI
```

## Site Categories & Modes

| Mode | Requirement | Examples |
|------|-------------|----------|
| **Public** | None | hackernews, devto, lobsters, stackoverflow, wikipedia, arxiv, bbc, steam, hf, apple-podcasts, sinafinance, linux-do |
| **Public/Browser** | Chrome open | google, v2ex, bloomberg |
| **Browser** | Chrome + extension | twitter, reddit, bilibili, zhihu, xiaohongshu, weibo, douban, weread, youtube, boss, facebook, instagram, tiktok, jike, medium, substack, linkedin, xueqiu, weixin, doubao, jimeng, yollomi |
| **Desktop** | App running | cursor, codex, notion, chatgpt, discord-app, chatwise, doubao-app, antigravity |

## Quick Decision Trees

### "I need content from a website"

```text
What site?
├─ Supported adapter exists → autocli <site> <command> --format json
├─ No adapter, article page → autocli read <url>
├─ No adapter, data page → autocli generate <url>
└─ Unsure → autocli list | autocli <site> --help
```

### "User wants to post / interact"

```text
What action?
├─ Post / publish / create → show content, get confirmation, then execute
├─ Reply / comment → show content + target, get confirmation
├─ Like / follow / bookmark → confirm target, execute
└─ Delete / unfollow → confirm target, warn irreversibility
```

### "Command doesn't exist"

```text
autocli <site> --help
├─ Command exists but different name → use correct name
├─ No commands for site → autocli generate <url>
│   ├─ Success → verify with autocli <site> <command> --format json
│   └─ Failure → create YAML adapter manually
└─ Still stuck → autocli doctor
```

## Self-Iteration (unsupported sites)

```bash
autocli <site> --help           # check if supported
autocli generate <url>         # auto-generate adapter
autocli explore <url>           # explore website APIs
autocli cascade <url>           # auto-detect auth strategies
```

Manual YAML adapter at `~/.autocli/adapters/<site>/`:

```yaml
site: sitename
name: command
description: description
domain: example.com
strategy: public
browser: true

args:
  limit:
    type: int
    default: 10

pipeline:
  - navigate: https://example.com
  - evaluate: |
      (async () => {
        return document.querySelectorAll('.item').map(el => ({
          title: el.textContent.trim()
        }));
      })()

columns: [title]
```

Debugging tips: find `data-test` attributes first (most stable), then class patterns, use `seen = new Set()` for dedup.

## Anti-Patterns

| Anti-pattern | Why it fails | Correct |
|---|---|---|
| Using playwright when autocli supports the site | Wastes time, ignores user's Chrome session | Try `autocli <site> <command>` first |
| Using `autocli read` when site-specific command exists | Loses structured data, slower | `autocli hackernews top` > `autocli read https://news.ycombinator.com` |
| Skipping confirmation on write ops | Risk of platform ban, irreversible actions | Always show content and wait |
| Hardcoding credentials in commands | Security risk, unnecessary | autocli reuses Chrome sessions |
| Saying "not supported" | Breaks core principle | `autocli generate` or manual YAML adapter |
| Guessing command names | Likely wrong, 333 commands exist | `autocli list` or `autocli <site> --help` |

## Discovery & Diagnostics

```bash
autocli list     # all 333 commands across 55+ sites
autocli --help
autocli doctor   # full diagnostics (Chrome, extension, session)
```

## Install (if missing)

```bash
curl -fsSL https://raw.githubusercontent.com/nashsu/AutoCLI/main/scripts/install.sh | sh
```

Windows: https://github.com/nashsu/AutoCLI

## Reference Index

| File | Purpose — load when... |
|---|---|
| `references/site-commands.md` | Need exact command names and args for a specific site |
| `references/browser-mode.md` | Setting up Chrome extension, troubleshooting session issues |
| `references/adapter-creation.md` | Creating YAML adapters for unsupported sites, DOM exploration patterns |
| `references/desktop-mode.md` | Desktop app integration (Cursor, Codex, Notion, etc.) |
| `references/ai-discovery.md` | `autocli explore`, `autocli cascade`, `autocli generate` deep dive |
