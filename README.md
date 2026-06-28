<div align="center">

<img src=".media/logo.jpg" alt="uwuwu-agent logo" width="120" />

# Uwuwu Agent

**AI agent skills, tools, and memory gateway for [opencode](https://opencode.ai)**

[![GitHub](https://img.shields.io/badge/GitHub-yurvon--screamo%2Fuwuwu--agent-181717?logo=github)](https://github.com/yurvon-screamo/uwuwu-agent)
[![opencode](https://img.shields.io/badge/opencode-AI%20coding%20agent-blueviolet?logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIxZW0iIGhlaWdodD0iMWVtIiB2aWV3Qm94PSIwIDAgMjQgMjQiPjxwYXRoIGZpbGw9IndoaXRlIiBkPSJNNy4yIDE3LjhsLTUuMi0zbC41LTguOUw3LjQgMy41TDkgOS41bC0uMiAyLjRsLTMuNiAyLjRsMy44IDIuMmwxLjQtMy4zem0xMC44IDBsNS4yLTNsLS41LTguOUwxNi42IDMuNUwxNSA5LjVsLjIgMi40bDMuNiAyLjRsLTMuOCAyLjJsLTEuNC0zLjN6bS00LjEtMS4ybC4zIDQuNWwtNC4yIDJ2LTUuOGw0LjktLjd6Ii8+PC9zdmc+)](https://opencode.ai)

</div>

---

## 📁 Structure

```
uwuwu-agent/
├── agent/              # Agent personas (developer, head-of-development, dreaming, office-coworker, ...)
├── skills/              # Modular skill library
│   ├── engineer-*/      #   Language-specific engineers (Rust, C#, Leptos, TS, ...)
│   ├── flow-*/          #   Workflow flows (feature, bug, refactor, audit, ...)
│   ├── office-coworker-*/ #   Office document automation (docx, xlsx, pptx, pdf)
│   ├── rules-*/         #   Quality rules (clean code, security, performance, ...)
│   └── tool-integration-*/ #   External tool integrations (GitLab, GitHub, Atlassian, browser)
├── tools/               # Runtime tool wrappers (memory, time, bg)
├── memory/              # TDAI memory gateway (TencentDB vector memory)
├── .qlty/               # Qlty quality hooks (pre-commit, pre-push)
├── start-memory.sh      # Memory gateway launch (Unix)
└── opencode.json        # opencode agent configuration
```

## 🧠 Memory Gateway

The memory subsystem runs a standalone HTTP sidecar based on [`@tencentdb-agent-memory/memory-tencentdb`](https://www.npmjs.com/package/@tencentdb-agent-memory/memory-tencentdb) — hybrid BM25 + vector search for persistent agent memory.

Memory data is stored **externally** at `~/uwuwu-memory-content/` (not in this repo).

```bash
# Start the memory gateway
./start-memory.sh
```

### Configuration

All secrets are read from **environment variables** — nothing is committed:

| Variable | Description |
|---|---|
| `OPENROUTER_API_KEY` | OpenRouter LLM API key |
| `GITLAB_PERSONAL_ACCESS_TOKEN` | GitLab PAT |
| `CONFLUENCE_PERSONAL_TOKEN` | Confluence API token |
| `JIRA_PERSONAL_TOKEN` | Jira API token |
| `Z_AI_API_KEY` | Z.AI image/video analysis key |

## 🎯 Skills Overview

### Engineers (6)
| Skill | Stack |
|---|---|
| `engineer-rust` | Rust |
| `engineer-csharp` | C# / .NET |
| `engineer-leptos` | Leptos (Rust WASM) |
| `engineer-typescript` | TypeScript |
| `engineer-python` | Python |
| `engineer-playwright` | Playwright E2E |

### Workflows (8)
`flow-feature` · `flow-bug` · `flow-refactor` · `flow-audit` · `flow-merge-review` · `flow-code-documentation` · `flow-agents-md-writer` · `flow-timesheet`

### Office Automation (4)
`office-coworker-docx` · `office-coworker-xlsx` · `office-coworker-pptx` · `office-coworker-pdf`

### Integrations (5)
`tool-integration-gitlab` · `tool-integration-github` · `tool-integration-atlassian` · `tool-integration-browser` · `tool-integration-research`

### Rules (9)
`rules-clean-code` · `rules-git-commit` · `rules-security` · `rules-performance` · `rules-qlty` · `rules-test-rule` · `rules-lib-usage` · `rules-text-writing` · `rules-ui`

### Generic Tools (1)
`tool-generic-likec4`

## ⚡ Quick Start

```bash
# Clone
git clone https://github.com/yurvon-screamo/uwuwu-agent.git
cd uwuwu-agent

# Install dependencies
npm install
cd memory && npm install && cd ..

# Set required environment variables
export OPENROUTER_API_KEY="your-key"

# Start memory gateway
./start-memory.sh

# Run with opencode
opencode
```

## 📋 License

This project is licensed under the [MIT License](LICENSE).
