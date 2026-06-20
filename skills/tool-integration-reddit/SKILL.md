---
name: tool-integration-reddit
description: "Reddit read-only monitoring: Tavily (site:reddit.com) → PRAW (OAuth script-app) → Playwright (logged-in Edge). Track mentions in r/LocalLLaMA, r/rust, competitive landscape, sentiment. Reddit .json endpoints DEAD since May 2026. NO auto-posting (ToS ban risk), NO vote manipulation. Triggers: 'reddit mentions', 'monitor r/LocalLLaMA', 'track reddit sentiment', 'search reddit', 'reddit competitor mentions'. Do NOT use for posting to Reddit — manual paste-ready copy only."
---

# Reddit (read-only monitoring)

Reddit closed public `.json` endpoints in **May 2026**. The only sanctioned access paths are: (1) Tavily search with `site:reddit.com`, (2) PRAW with OAuth script-app, (3) Playwright on a logged-in browser session. This skill covers all three. **Posting/voting through API is a ban risk** — `@marketer` produces paste-ready copy and the user publishes manually.

## Auth

Credentials live in `marketing/.env` (read by `@tool-accessor` only):

```
REDDIT_CLIENT_ID=...
REDDIT_CLIENT_SECRET=...
REDDIT_USER_AGENT=windows:uwuwu-marketer-monitor:v1.0.0 (by /u/turbin_y)
REDDIT_USERNAME=turbin_y
REDDIT_PASSWORD=...
```

**User-Agent format (Reddit requirement, do NOT change):**

```
<platform>:<app_id>:<version> (by /u/<username>)
```

- `<platform>` — `windows` / `linux` / `macos` / `ios` / `android`.
- `<app_id>` — descriptive app id (no spaces).
- `<version>` — semantic version.
- `(by /u/<username>)` — Reddit account that owns the script-app.

A non-conformant UA triggers throttling / shadowban.

## Tier 1 — Tavily (PRIMARY, no auth, simplest)

Use Tavily search scoped to reddit.com. Good for broad discovery and competitive research.

```bash
tvly search "site:reddit.com r/LocalLLaMA semantic memory proxy rust" \
  --max-results 10 --depth advanced --topic general --json
```

```bash
# Mentions of a specific project across all subreddits.
tvly search "site:reddit.com smos rust memory" \
  --max-results 20 --depth advanced --time-range month --json
```

```bash
# Competitive landscape — mentions of competitors.
tvly search "site:reddit.com (Mnemo OR Memzent OR Reflex OR linggen-memory OR mememory)" \
  --max-results 30 --depth advanced --time-range year --json
```

Use `--include-raw-content markdown` if you need the post body extracted.

## Tier 2 — PRAW (OAuth script-app, 100 QPM)

For structured monitoring (specific subreddit, user history, comment trees). PRAW = Python Reddit API Wrapper, OAuth **script-app** type, 100 queries-per-minute per OAuth client.

### Setup (one time)

```bash
# 1. Create script-app at https://www.reddit.com/prefs/apps
#    Type: script
#    redirect-uri: http://localhost:8080
# 2. Note client_id (under app name) and client_secret.
# 3. Fill marketing/.env (template in marketing/.env.example).
```

### Least-privilege auth (PREFERRED — read-only monitoring)

For **read-only monitoring** use application-only OAuth: only `client_id` + `client_secret` + `user_agent`. Do NOT store `REDDIT_PASSWORD` — `read_only = True` is a soft flag, not a technical enforcement; if `.env` leaks, an account password grants full write access despite the flag.

```python
import os, praw

# Application-only OAuth — NO password in .env for pure read-only monitoring.
reddit = praw.Reddit(
    client_id=os.environ["REDDIT_CLIENT_ID"],
    client_secret=os.environ["REDDIT_CLIENT_SECRET"],
    user_agent=os.environ["REDDIT_USER_AGENT"],
)
reddit.read_only = True  # monitoring only — never flip this off

# ⚠️ Limitation: application-only OAuth CANNOT read user-private scopes
# (e.g., reddit.redditor("turbin_y").submissions.new()). For that ONE use case
# (tracking turbin_y's own submission history) see the script-app auth example
# in the "Track user's own submissions" section below. Default to application-only.
```

### Mention monitoring

```python
TARGET_SUBREDDITS = ["LocalLLaMA", "rust", "MachineLearning", "Ollama"]
KEYWORDS = ["smos", "smos-rust", "semantic memory proxy", "memory proxy rust"]

for sub_name in TARGET_SUBREDDITS:
    for submission in reddit.subreddit(sub_name).search(
        " OR ".join(KEYWORDS), sort="new", time_filter="month", limit=50
    ):
        print(submission.created_utc, sub_name, submission.title, submission.url)
        submission.comments.replace_more(limit=0)
        for comment in submission.comments.list():
            if any(kw.lower() in comment.body.lower() for kw in KEYWORDS):
                print("  comment:", comment.body[:200])
```

### Sentiment / engagement snapshot

```python
for submission in reddit.subreddit("LocalLLaMA").top(time_filter="week", limit=20):
    print(
        submission.score,
        submission.num_comments,
        submission.upvote_ratio,
        submission.title,
    )
```

### Track user's own submissions (engagement follow-up)

> ⚠️ This call (`reddit.redditor(...).submissions.new()`) reads a user-private scope and REQUIRES script-app auth (username + password). The default application-only OAuth above will raise `MissingRequiredAttributeException`. Swap to script-app auth ONLY for this use case:

```python
# Script-app auth — needed ONLY for user-private scopes (e.g., own submission history).
# Application-only OAuth (the default above) is sufficient for everything else.
reddit = praw.Reddit(
    client_id=os.environ["REDDIT_CLIENT_ID"],
    client_secret=os.environ["REDDIT_CLIENT_SECRET"],
    user_agent=os.environ["REDDIT_USER_AGENT"],
    username=os.environ["REDDIT_USERNAME"],
    password=os.environ["REDDIT_PASSWORD"],
)
reddit.read_only = True  # still read-only — posting is always manual

for submission in reddit.redditor("turbin_y").submissions.new(limit=20):
    print(submission.subreddit, submission.score, submission.num_comments, submission.title)
```

## Tier 3 — Playwright (fallback, logged-in browser)

When Tavily returns nothing and PRAW hits rate limits / private subs. Use the `playwriter` CLI via `tool-integration-browser` skill — Edge profile `yurvon@yandex.ru` is already logged in.

```bash
# Read the browser skill docs first.
playwriter skill

# Reuse session, do NOT open a new one.
playwriter session list

# Navigate to a thread and extract comments.
playwriter -s <SESSION_ID> -e 'await page.goto("https://www.reddit.com/r/LocalLLaMA/comments/<id>/"); const text = await page.textContent("[data-testid=post-container]"); console.log(text)'
```

> Playwright is the LAST resort. Reddit's bot-detection flags automated navigation patterns — prefer Tavily/PRAW, use Playwright only for one-off manual reads of a specific thread.

## URL patterns

| Pattern | Purpose |
|---------|---------|
| `https://www.reddit.com/r/<sub>/search/?q=<query>&sort=new&t=month` | Web UI search (open for the user, not for scraping) |
| `https://www.reddit.com/r/LocalLLaMA/` | r/LocalLLaMA home (~600K members, local-LLM community) |
| `https://www.reddit.com/r/rust/` | r/rust home (~390K members, Rust language) |
| `https://www.reddit.com/r/Ollama/` | r/Ollama home (~100K members, local LLM runtime) |
| `https://www.reddit.com/r/MachineLearning/` | r/MachineLearning (~3M members, ML research) |
| `https://www.reddit.com/user/turbin_y/` | Author profile (track own submissions) |

## Subreddit cheat sheet (for smos-rust tier)

| Subreddit | Members | Relevance | Self-promo tolerance |
|-----------|---------|-----------|----------------------|
| r/LocalLLaMA | ~600K | **Primary** — local LLM infra, memory, OpenAI-compatible tools | Show&Tell flair allowed |
| r/rust | ~390K | **Primary** — Rust crates, architecture | Low — must be substantive |
| r/Ollama | ~100K | Secondary — Ollama users who benefit from memory layer | Medium |
| r/MachineLearning | ~3M | Tertiary — research audience, NLI contradiction-detection angle | Low — research-oriented |
| r/SillyTavernAI | — | **AVOID** — character chatbots, wrong audience | — |

## Anti-patterns (NEVER)

- **NEVER auto-post through API.** Reddit's Responsible Builder Policy (2025) + LLM-detection = perma-ban. Posting is **manual** — user copies paste-ready copy from `marketing/artifacts/`.
- **NEVER vote-manipulate.** No self-upvote bots, no friend-upvote coordination, no vote rings. Violates Reddit ToS, leads to shadowban.
- **NEVER use `.json` endpoints.** Dead since May 2026 — return 403/blocked. Use Tavily/PRAW/Playwright only.
- **NEVER scrape without UA** — non-conformant user-agent = shadowban.
- **NEVER train AI on Reddit content** without ToS compliance — Reddit data-licensing rules.
- **NEVER ignore 10:1 rule** — for every 1 self-promo, account must have 10 useful contributions in the same subreddit. Account `turbin_y` must build karma **before** first self-promo.

## Rules

- ALWAYS prefer Tavily for one-shot discovery; PRAW for structured monitoring; Playwright only as last-resort fallback.
- ALWAYS run PRAW in `read_only = True` mode.
- ALWAYS record snapshots with timestamps — Reddit edits/deletes are common, sentiment reports need provenance.
- ALWAYS respect `10:1 rule` — track contribution ratio before recommending any self-promo post.
- NEVER expose `REDDIT_PASSWORD` / `REDDIT_CLIENT_SECRET` in logs, reports, or `task` prompts.
- NEVER use this skill to POST or VOTE — only READ.
