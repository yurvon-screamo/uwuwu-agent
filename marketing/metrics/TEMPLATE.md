# Metrics — collection template

> Per-product metrics for the @marketer feedback loop. After each launch, snapshot metrics over a **24–72 hour window** — earlier signals are noise, later signals are stale. One file per ISO week per product: `metrics/<product>/YYYY-Www.md`.

## Window rule

| Stage | When | Action |
|-------|------|--------|
| Pre-publish | T−24h | Record baseline (stars, karma, page views, reactions) |
| Burst | T+0 to T+48h | Poll every 1h for the first 6h, then every 6h |
| Signal window | T+24h to T+72h | The window that decides success. Snapshot at T+24h, T+48h, T+72h. |
| Beyond | T+72h+ | Optional weekly cadence; not part of the launch decision. |

> Earlier than 24h = noise (initial friend/early-adopter bump). Later than 72h = decay phase; use only for trend, not for "did this launch work".

## Per-channel metrics

### GitHub

```bash
# Repo stats (stars, forks, subscribers).
gh api repos/yurvon-screamo/smos --jq '{stars: .stargazers_count, forks: .forks_count, subscribers: .subscribers_count, open_issues: .open_issues_count}'

# Traffic (last 14 days).
gh api repos/yurvon-screamo/smos/traffic/views --jq '{views: .count, unique: .uniques}'
gh api repos/yurvon-screamo/smos/traffic/clones --jq '{clones: .count, unique: .uniques}'
gh api repos/yurvon-screamo/smos/traffic/popular/referrers
```

| Metric | Source | Window |
|--------|--------|--------|
| Stars | `gh api repos/{o}/{r}` | T+0, T+24h, T+48h, T+72h |
| Forks | same | same |
| Traffic (views) | `gh api …/traffic/views` | last 14 days |
| Traffic (clones) | `gh api …/traffic/clones` | last 14 days |
| Referrers | `gh api …/traffic/popular/referrers` | tells you which channel drove traffic |

### Hacker News

```bash
# Algolia: find your Show HN post by title slug, then poll points/comments.
curl -s "http://hn.algolia.com/api/v1/search?query=SMOS%20OpenAI-compatible%20semantic%20memory&tags=show_hn" \
  | jq '.hits[0] | {objectID, points, num_comments, created_at}'

# Firebase: authoritative score (Algolia lags).
ID=<objectID from Algolia>
curl -s "https://hacker-news.firebaseio.com/v0/item/$ID.json" \
  | jq '{score, descendants, kids: (.kids | length)}'

# Rank: front-page position.
curl -s "http://hn.algolia.com/api/v1/search?tags=front_page&hitsPerPage=30" \
  | jq '[.hits[].objectID] | index("'$ID'")'
```

| Metric | Source | Window |
|--------|--------|--------|
| Points | Firebase `item/{id}` | T+6h, T+24h, T+48h |
| Comments (descendants) | Firebase `item/{id}` | T+24h, T+48h |
| Front-page rank | Algolia `tags=front_page` | T+6h, T+12h (decay is fast) |

### Reddit

```python
# Via PRAW (read-only). Application-only OAuth — NO password for read-only monitoring.
# Replace SUBMISSION_ID with the actual id after manual post.
import praw, os
reddit = praw.Reddit(
    client_id=os.environ["REDDIT_CLIENT_ID"],
    client_secret=os.environ["REDDIT_CLIENT_SECRET"],
    user_agent=os.environ["REDDIT_USER_AGENT"],
)
reddit.read_only = True

submission = reddit.submission(id="SUBMISSION_ID")
print(submission.score, submission.upvote_ratio, submission.num_comments)
submission.comments.replace_more(limit=0)
print(len(submission.comments.list()), "top-level + nested comments")
```

| Metric | Source | Window |
|--------|--------|--------|
| Upvotes (score) | PRAW `submission.score` | T+24h, T+48h |
| Upvote ratio | PRAW `submission.upvote_ratio` | T+24h |
| Comments | PRAW `submission.num_comments` | T+24h, T+48h |
| Sentiment (manual) | read comment bodies via PRAW | T+48h |

### dev.to

```bash
# Article metrics via authenticated GET.
ARTICLE_ID=1234567
curl -s -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     "https://dev.to/api/articles/$ARTICLE_ID" \
  | jq '{title, page_views_count, public_reactions_count, comments_count, published}'
```

| Metric | Source | Window |
|--------|--------|--------|
| Page views | `GET /articles/{id}` → `page_views_count` | T+24h, T+48h, T+72h |
| Reactions | same → `public_reactions_count` | T+24h, T+48h |
| Comments | same → `comments_count` | T+24h, T+48h |

### Product Hunt

```bash
# GraphQL v2: query post by slug for votes/comments/rank.
curl -s -X POST https://api.producthunt.com/v2/api/graphql \
  -H "Authorization: Bearer $PH_API_TOKEN" \
  -H "Content-Type: application/json" \
  -H "User-Agent: uwuwu-marketer (PH API v2)" \
  -d '{"query": "query { post(slug: \"smos-rust\") { votesCount commentsCount url featuredAt } }"}' \
  | jq
```

| Metric | Source | Window |
|--------|--------|--------|
| Votes | GraphQL `post(slug:).votesCount` | T+12h, T+24h (PH day ends at 12:01 AM PST) |
| Comments | GraphQL `post(slug:).commentsCount` | T+24h |
| Daily rank | GraphQL or web scrape `producthunt.com/leaderboard/daily/<date>` | End of launch day |

## File naming

```
metrics/<product>/YYYY-Www.md
```

ISO week. Example: `metrics/smos-rust/2026-W25.md` covers all smos-rust publications in ISO week 25 of 2026 (Mon 2026-06-15 → Sun 2026-06-21).

## File template

```markdown
# Metrics — <product> — ISO week <YYYY-Www>

## Snapshot summary

| Channel | Article/Post id | T+24h | T+48h | T+72h | Verdict |
|---------|-----------------|-------|-------|-------|---------|
| GitHub stars | yurvon-screamo/smos | _ | _ | _ | _ |
| HN points | objectID 39284765 | _ | _ | _ | _ |
| HN rank | same | _ | _ | _ | _ |
| r/LocalLLaMA score | submission_id | _ | _ | _ | _ |
| r/rust score | submission_id | _ | _ | _ | _ |
| dev.to views | article 1234567 | _ | _ | _ | _ |
| PH votes | smos-rust | _ | _ | _ | _ |

## What worked
- _ (channel + concrete result)

## What did NOT work
- _ (channel + concrete result + hypothesis why)

## Adjustments for next launch
- _ (voice / timing / channel / format change)

## memory_capture payload
- key insight: _ (1 sentence)
- repeat: _
- avoid: _
```

## Feedback loop

```
publish → wait 72h → snapshot metrics/<product>/YYYY-Www.md → analyze what worked / didn't → memory_capture (learnings) → update strategy.md if messaging change is warranted
```

`memory_capture` entries feed the next launch's pre-flight: the @marketer agent calls `memory_recall` at the start of each request and reads prior learnings. Voice drift, channel-mix tweaks, and timing adjustments propagate through this loop.
