---
name: tool-integration-devto
description: "dev.to (Forem) REST API for publishing technical articles and tracking metrics. Create/update articles, fetch page_views/reactions/comments, monitor comments. Header: api-key + accept: application/vnd.forem.api-v1+json. Max 4 lowercase tags. Save as draft first, never auto-publish without HUMAN GATE. Triggers: 'publish to dev.to', 'dev.to article', 'dev.to metrics', 'forem api', 'cross-post dev.to'. Do NOT use for Hashnode (Pro required since May 2026 — Phase 4 if needed)."
---

# dev.to (Forem REST API)

dev.to runs on Forem and exposes a public REST API at `https://dev.to/api`. Auth is a single `api-key` header. The API supports article CRUD, comment reading, and metrics. Account in scope: **yurvon-screamo** (verified to exist).

> **Hashnode is OUT of MVP.** Hashnode moved the free publishing API behind Pro tier in May 2026. Cross-posting to Hashnode is deferred to Phase 4 if/when Pro is acquired or the free tier is restored.

## Auth

Credentials live in `marketing/.env` (read by `@tool-accessor` only):

```
DEVTO_API_KEY=...
```

All authenticated requests require two headers:

```
api-key: <DEVTO_API_KEY>
accept: application/vnd.forem.api-v1+json
content-type: application/json
```

The `accept: application/vnd.forem.api-v1+json` header pins the API version — omitting it returns the latest (unstable) version and can break request shapes.

## Account

- Username: `yurvon-screamo`
- Base URL: `https://dev.to/api`
- Verify account/key works:

```bash
curl -s -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     https://dev.to/api/users/me | jq '{username, name, id}'
```

## Endpoints

### Create article — `POST /articles`

```bash
curl -s -X POST -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     -H "content-type: application/json" \
     https://dev.to/api/articles \
     -d '{
       "article": {
         "title": "Why I built a memory proxy with NLI contradiction detection",
         "published": false,
         "body_markdown": "---\ntitle: ...\npublished: false\ndescription: ...\ntags: rust, ai, llm, showdev\n---\n\n# Why NLI, not cosine\n\n...",
         "tags": ["rust", "ai", "llm", "showdev"],
         "series": null,
         "main_image": "https://example.com/cover.png",
         "canonical_url": "https://yurvon-screamo.github.io/smos/nli-vs-cosine"
       }
     }' | jq '{id, slug, url, published}'
```

> **HUMAN GATE rule:** `published: false` (draft) until the user explicitly approves. The user flips to `published: true` either in the dev.to UI or via `PUT /articles/{id}`.

### Update article — `PUT /articles/{id}`

```bash
# Publish a previously-saved draft after HUMAN GATE approve.
curl -s -X PUT -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     -H "content-type: application/json" \
     https://dev.to/api/articles/1234567 \
     -d '{"article": {"published": true}}' | jq '{published, url}'

# Edit body/tags.
curl -s -X PUT -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     -H "content-type: application/json" \
     https://dev.to/api/articles/1234567 \
     -d '{"article": {"body_markdown": "...new body...", "tags": ["rust","ai"]}}' | jq '{slug, url}'
```

### Get article (with metrics) — `GET /articles/{id}`

```bash
curl -s -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     https://dev.to/api/articles/1234567 | \
  jq '{title, page_views_count, public_reactions_count, comments_count, published}'
```

### List user's articles — `GET /articles/me`

```bash
curl -s -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     "https://dev.to/api/articles/me?per_page=50" | \
  jq '.[] | {id, title, page_views_count, public_reactions_count, comments_count, published}'
```

### Read comments — `GET /comments`

```bash
# Comments on a specific article (by article id).
curl -s -H "api-key: $DEVTO_API_KEY" \
     -H "accept: application/vnd.forem.api-v1+json" \
     "https://dev.to/api/comments?a_id=1234567" | \
  jq '.[] | {id_code, body_html, user: .user.username, created_at}'
```

### Unauthenticated read endpoints

```bash
# Any article by slug (no auth needed).
curl -s https://dev.to/api/articles/yurvonscreamo/why-i-built-a-memory-proxy | jq '{page_views_count, public_reactions_count}'
```

## Article body shape

dev.to uses **Jekyll-style frontmatter** inside `body_markdown`. The frontmatter and the JSON wrapper are redundant — set values in BOTH places (dev.to merges; JSON wins on conflict):

```markdown
---
title: "Why I built a memory proxy with NLI contradiction detection"
published: false
description: "Cosine similarity gave me avg 0.82 between contradicting fact pairs. Here's what I did about it."
tags: rust, ai, llm, showdev
cover_image: https://example.com/cover.png
canonical_url: https://yurvon-screamo.github.io/smos/nli-vs-cosine
---

# Why NLI, not cosine

Body in markdown. Supports code blocks with syntax highlighting...

```rust
// code here
```
```

## Rules

### Tags

- **Max 4 tags** per article.
- **Lowercase only.** ❌ `Rust`. ✅ `rust`.
- **No spaces.** Use hyphens: ✅ `machinelearning`, ✅ `ai`, ✅ `showdev`.
- Recommended for smos-rust: `rust`, `ai`, `llm`, `showdev` (or `opensource`).

### Images

- **NO image upload API.** dev.to does not provide a programmatic image-upload endpoint.
- Use **external URLs**: `cover_image: https://...` in frontmatter, `![alt](https://...)` in body.
- Host images on GitHub raw URLs, imgur, or your own CDN.

### Cross-posting (canonical_url)

- ALWAYS set `canonical_url` when the article is published elsewhere first (e.g., own blog).
- dev.to will then show "Originally published at <canonical_url>" and pass SEO to the original.
- NEVER omit `canonical_url` for cross-posts — dev.to outranks the original on Google, defeating the cross-post.

### Draft-first workflow

1. Create with `published: false` (draft).
2. User reviews in dev.to UI.
3. User approves → `PUT /articles/{id}` with `published: true`.
4. Track metrics via `GET /articles/{id}` over 24–72h.

## Anti-patterns (NEVER)

- **NEVER auto-publish (`published: true` on first POST) without HUMAN GATE approve.** Save as draft, wait for explicit user approval.
- **NEVER cross-post without `canonical_url`.** Google will rank the dev.to copy above the original, defeating the cross-post intent.
- **NEVER use more than 4 tags** — dev.to silently drops extras.
- **NEVER use mixed-case or spaced tags** — `Rust` and `machine learning` are silently normalized or dropped.
- **NEVER assume image upload works** — there is no upload API. Always pre-host images externally.
- **NEVER forget `accept: application/vnd.forem.api-v1+json`** — without it you may hit an unstable API shape.
- **NEVER echo `DEVTO_API_KEY` in logs, reports, or `task` prompts.**

## Rules (summary)

- ALWAYS set `published: false` on first create; flip to `true` only after explicit HUMAN GATE approve.
- ALWAYS include `accept: application/vnd.forem.api-v1+json` header.
- ALWAYS set `canonical_url` for cross-posted content.
- ALWAYS use lowercase, space-free tags (max 4).
- ALWAYS pre-host images externally — there is no upload API.
- NEVER commit `DEVTO_API_KEY` to VCS (it lives in `marketing/.env`).
