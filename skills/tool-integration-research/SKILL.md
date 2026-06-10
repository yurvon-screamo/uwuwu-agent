---
name: tool-integration-research
description: "Unified research skill: fetch library docs (ctx7), search code across GitHub (grep.app), search the web and extract URLs (Tavily). Use when you need to research libraries, frameworks, code examples, or general technical information. Triggers: 'docs for X', 'how does X work', 'search code', 'look up X online', 'search the web', 'find information', 'research topic', 'web search', 'look up online', 'find documentation'. Do NOT use for refactoring, business logic, or general programming concepts."
---

# CLI: research

Unified research skill using native CLI tools.

## Tools

### Context7 — library documentation

```bash
ctx7 library <name> [query]              # Resolve library name to Context7 ID
ctx7 library react "how to use hooks"
ctx7 docs <libraryId> <query>            # Fetch up-to-date docs for a library
ctx7 docs /facebook/react "useEffect examples"
```

Both commands support `--json` for machine-readable output.

### Grep — code search on GitHub

```bash
bun skills/tool-integration-research/scripts/grep.ts searchGitHub --query <pattern> [options]
```

- `--language <lang>` — filter by language (JavaScript, TypeScript, Python, etc.)
- `--use-regexp true` — enable regex mode (prefix with `(?s)` for multiline)
- `--repo <owner/repo>` — restrict to specific repo
- `--match-case true` — case-sensitive search

**Important:** Searches for literal code patterns, not keywords. Search for actual code, not descriptions.

### Tavily — web search, extract, crawl, map

```bash
# Search
tvly search <query> [--max-results N] [--depth basic|advanced] [--topic general|news|finance]
                    [--time-range day|week|month|year] [--include-domains DOMAINS]
                    [--exclude-domains DOMAINS] [--include-answer basic|advanced]
                    [--include-raw-content markdown|text] [--json]

# Extract content from URLs (up to 20)
tvly extract <URL> [<URL>...] [--query TEXT] [--format markdown|text] [--json]

# Crawl website
tvly crawl <URL> [--max-depth N] [--limit N] [--instructions TEXT] [--json]

# Discover URLs on a website
tvly map <URL> [--max-depth N] [--limit N] [--instructions TEXT] [--json]

# Deep research (async)
tvly research run <query>
tvly research status <id>
tvly research poll <id>
```

## When to Use Which Tool

| Question type | Tool | Why |
|---|---|---|
| "How does X work?", "docs for Y" | **ctx7** | Curated docs with explanations |
| "Best practices for X" | **ctx7** | Official documentation sources |
| "How do people actually use Z?" | **grep** | Real code from public repos |
| "Find calls to `someObscureApi()`" | **grep** | Literal code search, regex support |
| "What is X?", general lookup | **tvly search** | Web search with AI answer |
| "Read this URL" | **tvly extract** | Extract clean content from any page |
| "Crawl this documentation site" | **tvly crawl** | Multi-page content extraction |
| "Deep research on topic X" | **tvly research** | Multi-step AI research |

**ctx7 vs grep rule of thumb:**
- `ctx7` = natural language question → curated answer with context
- `grep` = literal code pattern → raw real-world usage examples
- When in doubt, start with `ctx7`. Fall back to `grep` only when you need to see how real projects integrate something.

## Research Methodology

### Step 1: Analyze the query

Before searching, determine:
- What is the core question?
- What type of information is needed (factual, comparative, best practices, code examples)?
- What level of detail is appropriate?

### Step 2: Strategic search

1. Start with **ctx7** for library/framework questions ("how does X work?")
2. Use **tvly search** for broad discovery or non-library questions
3. Use **grep** when you need real-world code patterns, not docs
4. Use **tvly extract** to get clean content from promising URLs

### Step 3: Synthesize

1. Filter out irrelevant, outdated, or low-quality information
2. Cross-check conclusions from multiple sources when possible
3. Prioritize official documentation and authoritative sources
4. Extract only information that directly answers the query

## Report Format

### Successful research

```
## Research Summary
[1-2 sentences directly answering the core question]

## Key Findings
- [Finding 1 with source]
- [Finding 2 with source]
- [Finding 3 with source]

## Details
[Expanded explanation, only if necessary]

## Sources
- [Source Name 1](url)
- [Source Name 2](url)
```

### Failed research

```
## Research Status: Unable to complete

## What was attempted
[Brief description of search attempts]

## Why it failed
[Clear explanation — no results, conflicting info, paywalled content, etc.]

## Recommendations
- [Alternative search queries to try]
- [Suggested query modifications]
- [Alternative resources to consult]
```

## Quality Standards

- Every sentence must carry value — no filler
- Say it once, say it clearly — no repetition
- Strictly on topic — no tangential info
- Objective facts only — no marketing language
- Each report is self-contained — no assumed context

## Error Handling

When research hits obstacles:
1. Try alternative search strategies before reporting failure
2. If info is partial, report what was found and clearly state limitations
3. Suggest concrete next steps the user can take
4. Never fabricate or guess information

## Language

Respond in the same language the user used in your query. Russian query → Russian response. English query → English response.
