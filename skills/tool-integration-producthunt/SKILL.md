---
name: tool-integration-producthunt
description: "Product Hunt GraphQL v2 API (READ-ONLY for third parties). Query posts (by topic/date/featured/order), post by slug, topics, users. Submit/comment/vote = browser via tool-integration-browser (API is read-only). OAuth2 Developer Token. Rate: 6,250 complexity per 15 min. Maker comment template + launch playbook included. Triggers: 'product hunt', 'PH launch', 'PH metrics', 'graphql ph', 'producthunt post'. Do NOT use for automated submission — browser paste-ready copy only."
---

# Product Hunt (GraphQL v2 — read-only)

Product Hunt's GraphQL v2 API is **READ-ONLY for third-party apps**. Submitting posts, commenting, and voting must be done through the browser (`tool-integration-browser`, Edge profile `yurvon@yandex.ru`) — manual paste-ready copy from `@marketer`. This skill covers read queries (post lookup, topic trends, competitive analysis) and encodes the launch playbook + maker comment template.

## API

| Property | Value |
|----------|-------|
| Endpoint | `https://api.producthunt.com/v2/api/graphql` |
| Auth | OAuth2 — `Authorization: Bearer <PH_API_TOKEN>` (Developer Token) |
| Headers | `Authorization`, `Content-Type: application/json`, `User-Agent: uwuwu-marketer (PH API v2)` |
| Rate limit | **6,250 complexity points per 15 min** per token |
| Read/Write | **READ-ONLY** for third parties. Submit/comment/vote = browser via Playwright. |

## Auth

Credentials live in `marketing/.env` (read by `@tool-accessor` only):

```
PH_API_TOKEN=...        # OAuth2 Developer Token (long-lived)
PH_CLIENT_ID=...
PH_CLIENT_SECRET=...
```

Issue a Developer Token at `https://www.producthunt.com/v2/oauth/applications` (create app → generate token). The Developer Token is long-lived and bypasses the OAuth flow — ideal for a single-user monitoring use case.

> **User-Agent requirement:** PH requires a descriptive UA. Use `uwuwu-marketer (PH API v2)` — bare `curl/7.x` defaults may be throttled.

## Example query shell

```bash
curl -s -X POST https://api.producthunt.com/v2/api/graphql \
  -H "Authorization: Bearer $PH_API_TOKEN" \
  -H "Content-Type: application/json" \
  -H "User-Agent: uwuwu-marketer (PH API v2)" \
  -d '{"query": "<graphql query here>"}' | jq
```

## Common queries

### Get a post by slug (post-launch metric tracking)

```graphql
query {
  post(slug: "smos-rust") {
    id
    name
    tagline
    votesCount
    commentsCount
    url
    website
    createdAt
    featuredAt
    topics { edges { node { name } } }
    makers { name }
  }
}
```

```bash
curl -s -X POST https://api.producthunt.com/v2/api/graphql \
  -H "Authorization: Bearer $PH_API_TOKEN" \
  -H "Content-Type: application/json" \
  -H "User-Agent: uwuwu-marketer (PH API v2)" \
  -d '{"query": "query { post(slug: \"smos-rust\") { name votesCount commentsCount url featuredAt } }"}' | jq
```

### Posts by topic, date, order

```graphql
query {
  posts(order: RANKING, first: 20) {
    edges {
      node {
        name
        tagline
        votesCount
        commentsCount
        url
        topics { edges { node { name } } }
        createdAt
      }
    }
  }
}
```

`order` values: `RANKING` (today's leaderboard), `NEWEST`, `FEATURED`.

### Topic lookup

```graphql
query {
  topic(slug: "developer-tools") {
    name
    followersCount
    postsCount
    description
  }
}
```

### User profile (track own launches)

```graphql
query {
  user(username: "turbin_y") {
    name
    headline
    followersCount
    posts { edges { node { name votesCount url } } }
  }
}
```

## Topics cheat sheet (for smos-rust)

| Topic slug | Name | Followers (approx.) | Fit |
|------------|------|----------------------|-----|
| `developer-tools` | Developer Tools | ~514K | **Primary** |
| `artificial-intelligence` | Artificial Intelligence | ~471K | **Primary** |
| `github` | GitHub | ~41K | Secondary |
| `open-source` | Open Source | ~150K | Secondary |
| `rust` | Rust | ~30K | Tertiary (narrow) |
| `no-code` | No-Code | — | **AVOID** (wrong audience) |

Submit with 3–4 topics: `developer-tools`, `artificial-intelligence`, `open-source`, (optional) `rust`.

## Launch playbook (browser-paste-ready)

PH launch is **manual** via `tool-integration-browser` (Edge profile). The @marketer produces the paste-ready copy; the user submits in browser.

### Timing

- **Launch day: Tue / Wed / Thu.** ❌ Fri–Mon (low traffic, drops off leaderboard fast).
- **Launch time: 12:01 AM PST** = earliest allowed submission = maximum day-of runway.
- **Maker present all day** — answer every comment within 30 min for the first 8 hours.

### Gallery

- Cover gallery image: **635×380 px** (PH's standard). Smaller = blurry; larger = auto-cropped.
- First gallery image = the thumbnail shown on leaderboard. Make it readable at 80×60.
- 4–6 gallery images total (alternatives shown on hover).

### Maker comment template (post immediately after launch)

PH "maker comment" is the equivalent of Show HN first comment. Post within minutes of submission. Emoji are OPTIONAL and depend on the per-launch tone call — the @marketer default is plain text (matches brand voice "calm conviction, NOT enthusiasm"). Drop emoji unless the user explicitly asks for a warmer social-channel tone.

```
Hey PH — [maker name] here, maker of <Name>.

The problem.
<1–2 sentences: technical problem you hit.>

The gap.
<Existing solutions fall short because: ...>

The solution.
<What <Name> does, concretely. One sentence, no superlatives.>

Benefits.
- <benefit 1 with concrete number>
- <benefit 2>
- <benefit 3>

Proof.
<Test coverage / benchmarks / GitHub stars — concrete numbers.>

Offer.
<Discount / free tier / open-source link for PH community.>

Happy to answer any questions all day — I'll be here.
```

### Pre-launch checklist

- [ ] Gallery images: 635×380, 4–6 images, first one leaderboard-readable.
- [ ] Tagline ≤ 60 chars (PH truncates).
- [ ] Topics: `developer-tools`, `artificial-intelligence`, `open-source`, (`rust`).
- [ ] Maker comment written, ready to post within minutes of submission.
- [ ] Schedule: Tue/Wed/Thu, 12:01 AM PST.
- [ ] Maker cleared calendar for launch day (8h of comment-response availability).
- [ ] GitHub release + Show HN + Reddit + dev.to coordinated **within 48h burst**.

## Anti-patterns (NEVER)

- **NEVER ask for upvotes.** PH bans for explicit upvote solicitation ("if you like it, please upvote", mass DMs, Slack/Telegram/Discord campaigns).
- **NEVER run vote rings.** Coordinated voting (friends, colleagues, paid services) = perma-ban for the product AND the maker account.
- **NEVER launch Fri–Mon.** Engagement drops 40–60% vs Tue–Thu.
- **NEVER use a company/team account as maker.** PH requires an individual maker account; team accounts get demoted.
- **NEVER submit via API** — the API is read-only. Any browser-automation submit attempt risks account ban.
- **NEVER exceed rate limit** (6,250 complexity / 15 min) — use `first:` pagination; avoid deep nested queries.
- **NEVER echo `PH_API_TOKEN` in logs, reports, or `task` prompts.**

## Rules (summary)

- ALWAYS use GraphQL v2 with `Authorization: Bearer <PH_API_TOKEN>` and a descriptive User-Agent.
- ALWAYS poll `post(slug:)` for `votesCount`/`commentsCount` over 24–72h after launch.
- ALWAYS produce paste-ready gallery/tagline/maker-comment for the user; user submits manually in browser.
- ALWAYS launch Tue–Thu, 12:01 AM PST.
- NEVER ask for upvotes, NEVER run vote rings, NEVER use team account as maker.
- NEVER commit `PH_API_TOKEN` to VCS (it lives in `marketing/.env`).
