---
name: tool-integration-hn
description: "Hacker News read-only monitoring via Algolia Search API (PRIMARY) + Firebase HN API (SECONDARY). Track Show HN posts, comments, points, rank; monitor product mentions; competitive analysis. NO WRITE API exists — HN submission/commenting is manual via news.ycombinator.com. Triggers: 'show hn monitoring', 'track hn post', 'hn rank', 'hn competitor mentions', 'algolia search', 'firebase hn'. Do NOT use for posting to HN — manual paste-ready copy only."
---

# Hacker News (read-only)

Hacker News has **NO write API** — submission, commenting, and voting are manual via `https://news.ycombinator.com`. This skill covers read-only monitoring through two complementary read APIs: **Algolia Search API** (PRIMARY — full-text search, tags, time filters) and **Firebase HN API** (SECONDARY — live item/user details, comment trees).

## APIs

| API | Base URL | Auth | Use for |
|-----|----------|------|---------|
| **Algolia HN Search** (PRIMARY) | `http://hn.algolia.com/api/v1/` | None (free, public) | Full-text search, tags (`story`/`comment`/`show_hn`/`front_page`), time filters, sorting |
| **Firebase HN API** (SECONDARY) | `https://hacker-news.firebaseio.com/v0/` | None (free, public) | Live item/user fetch, comment trees, exact point/comment counts |

## Tier 1 — Algolia (PRIMARY search)

### Endpoints

```
GET /search            # full-text search across stories + comments
GET /search_by_date    # same, sorted by date (newest first)
GET /items/{id}        # full item tree (Algolia's view)
GET /users/{username}  # user profile
```

### Search parameters

| Param | Values | Notes |
|-------|--------|-------|
| `query` | string | Full-text query (Boolean `AND`/`OR`/`NOT`, parentheses) |
| `tags` | `story` / `comment` / `show_hn` / `front_page` / `(story,author_x)` | Comma = AND; parentheses = grouping |
| `numericFilters` | `created_at_i>=TIMESTAMP` | ⚠️ Only `created_at_i` works — `points`/`num_comments` filters **DO NOT** work despite docs |
| `hitsPerPage` | int | Default 20 |
| `storyText` | bool | Search only story body (not comments) |

> **Known bug:** `numericFilters=points>=50` returns zero hits. Algolia's index does not support filtering on `points` or `num_comments`. Filter client-side after fetching.

### URL patterns

```bash
# Search for product mentions in Show HN posts, last 30 days.
curl "http://hn.algolia.com/api/v1/search?query=smos&tags=story&numericFilters=created_at_i>$(($(date +%s)-2592000))" | jq '.hits[] | {title, points, num_comments, url}'

# Show HN posts only.
curl "http://hn.algolia.com/api/v1/search?tags=show_hn&hitsPerPage=30" | jq '.hits[] | {title, points, author}'

# Comments mentioning a project.
curl "http://hn.algolia.com/api/v1/search?query=smos&tags=comment" | jq '.hits[] | {comment_text, points, story_title}'

# Competitive analysis — track Mem0 / Letta / Cognee / Zep launches.
curl "http://hn.algolia.com/api/v1/search?tags=story&query=(Mem0%20OR%20Letta%20OR%20Cognee%20OR%20Graphiti%20OR%20Zep)" | jq '.hits[] | {title, points, num_comments, created_at}'

# A specific user's submissions.
curl "http://hn.algolia.com/api/v1/search?tags=(story,author_turbin_y)" | jq '.hits[] | {title, points}'
```

### Track own Show HN performance (post-launch)

```bash
# Find your Show HN post by title slug.
curl "http://hn.algolia.com/api/v1/search?query=SMOS%20OpenAI-compatible%20semantic%20memory%20proxy&tags=show_hn" | jq '.hits[0] | {objectID, points, num_comments, created_at}'

# Then poll points/comments over the next 48h (the burst window).
ID=39284765
while true; do
  curl "http://hn.algolia.com/api/v1/items/$ID" | jq '{points, num_comments, updated_at: now}'
  sleep 3600  # 1h
done
```

### Rank tracking (front page position)

```bash
# Fetch current front page, find your post's position.
curl "http://hn.algolia.com/api/v1/search?tags=front_page&hitsPerPage=30" | \
  jq '[.hits[] | .objectID] | index("39284765")'
```

## Tier 2 — Firebase HN API (SECONDARY, live details)

### Endpoints

```
GET /v0/topstories.json       # array of ~500 item ids, top stories right now
GET /v0/newstories.json       # ~500 newest
GET /v0/beststories.json      # ~200 best
GET /v0/showstories.json      # ~200 newest Show HN
GET /v0/askstories.json       # ~200 newest Ask HN
GET /v0/item/<id>.json        # full item: type, by, title, url, text, score, descendants, kids[]
GET /v0/user/<username>.json  # user profile: karma, about, submitted[]
```

### Patterns

```bash
# Live top-stories rank of your post (use the id from Algolia).
ID=39284765
curl "https://hacker-news.firebaseio.com/v0/topstories.json" | \
  python -c "import sys, json; ids = json.load(sys.stdin); print(ids.index($ID) + 1 if $ID in ids else 'not on top')"

# Full comment tree of your Show HN post.
curl "https://hacker-news.firebaseio.com/v0/item/$ID.json" | \
  jq '{score, descendants, kids: (.kids | length)}'

# Track your karma over time.
curl "https://hacker-news.firebaseio.com/v0/user/turbin_y.json" | jq '{karma, submitted: (.submitted | length)}'
```

> Firebase gives the **authoritative** `score` and `descendants` count. Algolia may lag by minutes. Use Firebase for final metrics snapshots.

## Show HN etiquette (encode into paste-ready copy)

Show HN posts must follow the format enforced by community norms (violations = downvote death):

### Title format

```
Show HN: <Name> – <one-line technical description>
```

- En-dash `–` (U+2013), not hyphen, between name and description.
- ≤ 80 characters (HN truncates longer).
- Plain description, no marketing words. ❌ `Show HN: Meet SMOS`. ✅ `Show HN: SMOS – OpenAI-compatible semantic memory proxy (Rust)`.
- No emoji. No "introducing". No "announce".

### First-comment templates (author posts immediately after submit, 300–500 words)

```
The problem.
<1–2 sentences: technical problem you hit. Concrete, not abstract.>

What I built.
<One sentence: what SMOS is, no adjectives.>

Architecture.
<3–5 lines: key technical decisions with numbers.>
Hexagonal/DDD three-crate workspace. Embedded SurrealDB (RocksDB), no external DB
process. NLI contradiction detection via DeBERTa-v3 sidecar — cosine similarity
averaged 0.82 between contradicting fact pairs in my fixture corpus, so I didn't
trust it for verdicts.

Benchmarks.
| Tier | Command | Tests | Warm time |
|------|---------|-------|-----------|
| Fast | cargo t | 533 | ~2.6s |
| Integration | cargo ti | 643 | ~50s |

Known limitations.
- CORS is permissive by default (safe on localhost; add [server].allowed_origins for non-localhost).
- find_session is O(N) snapshot_all — fine for thousands of sessions.
- Cosine only feeds candidate selection; HNSW cannot push equality filters (Rust post-filter).

Source: https://github.com/<owner>/<repo>

Happy to answer questions.
```

### Show HN operational checklist

- [ ] Submit Tue/Wed/Thu between **7–9 AM EST** (US East workday start = peak traffic).
- [ ] Author present at keyboard for the first **2 hours** — answer every comment.
- [ ] Link to GitHub repo, **NOT** a landing page.
- [ ] First comment (above) posted within seconds of submission.
- [ ] Title ≤ 80 chars, en-dash, no marketing words, no emoji.

## Anti-patterns (NEVER)

- **NEVER attempt to POST/COMMENT/VOTE via API** — HN has no write API. All interaction is manual via browser. `@marketer` produces paste-ready copy; the user submits.
- **NEVER rely on `numericFilters=points>=N`** — returns zero hits. Filter client-side.
- **NEVER use Algolia as the source of truth for final score** — it lags Firebase. Use Firebase for metric snapshots.
- **NEVER submit on Friday/weekend** — engagement ~50% lower, drops off front page faster.
- **NEVER use marketing language in title** — "meet X", "introducing X", "announcing X" = downvote death.
- **NEVER link to landing page** — HN audience downvotes anything that isn't source/docs.

## Rules

- ALWAYS use Algolia for search/discovery (Boolean queries, tags, time filters).
- ALWAYS use Firebase for authoritative score/comment counts in metric snapshots.
- ALWAYS record Show HN `objectID` immediately after manual submission — needed for all subsequent tracking.
- ALWAYS poll Algolia `items/{id}` for rank/points/comments over 48h burst window.
- NEVER expose HN account password in logs (login is browser-only, no API credential).
- NEVER attempt automated submission — there is no API, and any browser-automation attempt risks account ban.
