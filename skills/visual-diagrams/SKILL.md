---
name: visual-diagrams
description: Diagrams and visual explanations — mermaid and static SVG/HTML in uwuwu (hub: issues render mermaid natively, docs/*.md in uwuwu-hub render HTML/SVG as a file, JS is not executed anywhere). Use when a diagram/visualization is needed — on user request, when the text is unclear, for architecture conclusions.
---

# Visual Diagrams

Form follows content. This is not «boxes with arrows» — it is the full expressiveness of the web inside a uwuwu doc.

## How to write

- **Node graphs (architectures, pipelines, links)** → static **SVG/inline-HTML** (a dagre layout by hand or a visual editor; JS engines are not executed anywhere in the contour)
- Text flowcharts/sequences → **mermaid** (inline; GitHub renders mermaid fences in issues natively)
- Take `assets/template-vesper.html` (the theme for flat HTML) — the palette + typography; the layout and shapes are your own, driven by the content
- No JS/CDN/animations: GitHub sanitizes html — script is not executed, do not build in interactivity
- Where to insert: into the **issue body** — a mermaid fence or a picture (asset → raw URL); into a **wiki doc** (docs/*.md in uwuwu-hub) — an HTML/SVG block renders natively when viewing the file

## Two layers of visuals (the ontology of choice)

The engine choice = the question «which layer», not «which library»:

1. **A visual for the moment** — explain a specific thing now: a sequence in a task body, a mechanism in an experience article, an incident in a runbook. Engines: mermaid (sequences/flows), static SVG/inline-HTML (node graphs, wiki docs), bare Vesper HTML (timelines/custom, wiki docs). Never updated — goes stale together with its context, by design; the creation threshold is minimal.
2. **The project model (C4)** — the canonical description of structure, lives for years: element, link, deployment level. Only LikeC4: the «Модель (LikeC4)» doc in the project folder (the source of truth), in the Stack doc — a visualization + a link to the model. Updating: a DSL edit → `likec4 validate`; the view render — PNG export (see the LikeC4 section). Stable element ids = links between views and docs.

Cytoscape embeds of the affinity era in Stack docs are dead since the 04.11 migration (JS is not executed) — on doc revision they are replaced with static SVG or a visualization per the LikeC4 canon (precedent: true-washing); do not create new cytoscape diagrams.

## LikeC4: model-as-code in the wiki (the canon is alive, the interactive view is dead, PNG render is alive)

When architecture = **a model** (stable element ids, several linked views, a deployment level, static links) — not a one-off diagram. DSL sources live **in the wiki** (docs/ of the uwuwu-hub repo).

- **Storage (canon)**: the «Модель (LikeC4)» doc in the project folder; sources — ` ```c4 ` fences (specification/model/views/deployment as separate sections). In the Stack doc — a visualization + a link to the model doc. The ban — on manual copies and a second source of edits: the DSL is edited only in the wiki. A slice generated from the model (a one-way sync into the repo, not hand-edited) — not a second source but the standard pattern (ADR-013 true-washing).
- **LikeC4 CLI** (live-check 08.11.2026): `bun add -g likec4@1.60.0` works without a browser — `likec4 validate` and `likec4 format` (a probe on the true-washing model: validate validates 4 files, format fixes formatting). The procedure: export the model doc (MCP `uwuwu_docs_read`), cut the c4 fences into `*.c4` files of the folder, run `likec4 validate`. The DSL is the source of truth of the architecture.
- **The interactive view is dead**: the webcomponent (`<likec4-view>` + wc.mjs, React inside) requires JS — GitHub html does not execute it; LikeC4 interactivity (view navigation, element-details) is unavailable. The old `tools/likec4_refresh.py` pipeline was removed by purge commit d63cdc4. Existing likec4 embeds (the true-washing Stack doc) — dead since the 04.11 migration.
- **PNG export is alive** (live-check 09.11.2026, spike 20261108104457-6wsptcd): in the `*.c4` folder — `env -u OPENROUTER_API_KEY likec4 export png --dark -o png-out`; playwright (a direct dep of likec4) starts chromium from `~/.cache/ms-playwright`, a probe on the true-washing model — 4 views → 4 valid PNGs in ~40s (dark, up to 7028px wide). **The silent-failure trigger — AI keys in env** (`OPENROUTER_API_KEY`, plus OPENAI/ANTHROPIC/GEMINI/OLLAMA_HOST/MINIMAX): the preview server's AI feature activates → likec4 requires `@tanstack/ai`, unresolvable from cwd (a bare folder without node_modules) → «Package manager not detected» → `exit(1)` with no PNGs and no clear error about playwright. In an env without these keys — `env -u` is not needed. Diagnosis and procedure — the experience doc `cli/LikeC4 CLI & DSL Authoring` (the «Экспорт PNG» section); the PNG is inserted into a doc/task as an asset file.
- **Practice now**: the visualization in the Stack doc — static SVG/inline-HTML after the DSL (see «How to write»), insertion — html into docs/*.md (rendered as a file) or a picture in an issue; the model doc — the source of truth, view sync by hand.
- Links: ADR-052 uwuwu (html embeds), the experience doc «AFFiNE embed_html: живые HTML-диаграммы в контуре вики».

## Capabilities (hub, live-check 11.2026)

- **docs/*.md uwuwu-hub**: GitHub renders md files with inline-HTML/SVG/`<div>` layouts (sanitization: no script)
- **issue bodies**: mermaid — natively; inline-HTML is sanitized harder (styles are cut) — diagrams in issues via mermaid or a picture
- **JS is not executed anywhere** — diagrams are static; animations/steppers/CDN (unpkg/jsdelivr) — do not work, do not build them in

## The home palette — Vesper (uwuwu runs dark)

| Role | Hex |
|---|---|
| background | `#101010` |
| panel | `#181818` / `#1E1E1E` |
| lines | `#2E2E2E` / `#505050` |
| text | `#FFFFFF` · muted `#A0A0A0` · dim `#7E7E7E` |
| peach (the main accent) | `#FFC799` |
| mint | `#99FFE4` |
| lavender | `#ACA1CF` |
| rose | `#F5A191` |
| gold | `#FFCFA8` |

The palette is the home default; specific content may require its own.

## Survival rules (the only constraints)
