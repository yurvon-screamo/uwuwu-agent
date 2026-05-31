---
name: tool-integration-research
description: "Unified research skill: fetch library docs (Context7), explore GitHub repos (DeepWiki), search code across GitHub (grep.app), search the web (DuckDuckGo), read URLs. Use when you need to research libraries, frameworks, repos, code examples, or general technical information. Triggers: 'docs for X', 'how does X work', 'search code', 'look up X online', 'wiki for repo X', 'search the web', 'find information', 'research topic', 'web search', 'look up online', 'find documentation'. Do NOT use for refactoring, business logic, or general programming concepts."
---

# CLI: research

Unified research skill combining four backends via CLI scripts. Each script wraps an MCP backend.

## Scripts

### Context7 — library documentation

```bash
bun skills/cli-research/scripts/context7.ts <command> [options]
```

- `resolve-library-id` — resolve library name to Context7 ID
- `query-docs` — fetch up-to-date docs for a library

### DeepWiki — GitHub repo documentation

```bash
bun skills/cli-research/scripts/deepwiki.ts <command> [options]
```

- `ask-question` — ask a question about a GitHub repo
- `read-wiki-contents` — read full wiki docs for a repo
- `read-wiki-structure` — list documentation topics for a repo

### Grep — code search on GitHub

```bash
bun skills/cli-research/scripts/grep.ts <command> [options]
```

- `searchGitHub` — search literal code patterns across public GitHub repos

### Web Search — DuckDuckGo

```bash
bun skills/cli-research/scripts/websearch.ts <command> [options]
```

- `search` — search the web via DuckDuckGo
- `fetch-content` — fetch and extract text content from a URL

Run any script with `--help` to see available commands and flags.

## Research Methodology

### Step 1: Analyze the query

Before searching, determine:
- What is the core question?
- What type of information is needed (factual, comparative, best practices, code examples)?
- What level of detail is appropriate?
- Are there time constraints (fresh info vs. historical)?

### Step 2: Strategic search

1. Start with **websearch** (`search`) for broad discovery
2. Use **context7** (`query-docs`) for library/framework documentation
3. Use **deepwiki** (`ask-question`) for deep dive into specific repos
4. Use **grep** (`searchGitHub`) to find specific code patterns or usage examples
5. Use **websearch** (`fetch-content`) to extract content from promising URLs

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

Respond in the same language the user used in their query. Russian query → Russian response. English query → English response.
