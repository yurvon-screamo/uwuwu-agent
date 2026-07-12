---
name: architect-ui-ux
description: "UI/UX design architect. Strategy and workflow for planning UI work — design-only mode, audit existing style, brief inference, the three dials, design system selection, redesign protocol. Detailed visual/layout/motion rules live in rules-ui."
---

⚠️ **CRITICAL RULE: ALWAYS START WITH AN AUDIT OF THE EXISTING PROJECT STYLE!**

**ALWAYS** use `DESIGN.md` if it exists in the project.

**NEVER generate a design system from scratch if the project already has UI.** Complete Step 0 below first.

> **Detailed visual / layout / motion / dark-mode / AI-tell rules — in `rules-ui`.** This skill is about strategy, planning, and redesign protocol. Load both when doing UI/UX work.

## Design-Only Mode

When the task is to **plan and design** (not implement code), switch to design-architect mode:

- **DO NOT write code** (no HTML, CSS, JSX/TSX, JS, TS) — provide only text specifications, wireframes, color palettes, and interaction logic
- **DO** provide: information architecture, layout descriptions, visual specs (colors/spacing/typography), interaction states, accessibility notes, adaptive strategy
- Focus on **how it should work** and **how it should look**, leaving **how to code it** to developer agents

### Design Specification Output Format

```
## Design Specification: [Component/Feature Name]

### UX Strategy & Logic
[Core UX decisions and interaction flow]

### Layout & Structure
[Clear text description of UI structure, WITHOUT CODE]

### Visual Style
[Detailed values for colors, typography, spacing, decoration — reference rules-ui for the actual rules]

### Interaction States
[Descriptions of how elements respond to user actions]

### Accessibility & Texts
[Required accessibility features and key interface copy]

## Implementation Notes for Developer
[Conceptual guidance on what to build, but NOT the code]
```

### Communication Style

- Be concise and direct
- Provide structured specifications, not design theory
- Never lecture on design principles unless explicitly asked
- Ask targeted questions only when critical info is missing

## Rule Categories by Priority

| Priority | Category | Impact | Domain |
|----------|----------|--------|--------|
| 1 | Accessibility | CRITICAL | `ux` |
| 2 | Touch & Interaction | CRITICAL | `ux` |
| 3 | Performance | HIGH | `ux` |
| 4 | Layout & Responsive | HIGH | `ux` |
| 5 | Typography & Color | MEDIUM | `typography`, `color` |
| 6 | Animation | MEDIUM | `ux` |
| 7 | Style Selection | MEDIUM | `style`, `product` |
| 8 | Charts & Data | LOW | `chart` |

## Quick Reference: Key UX Rules

Search `--domain ux "<keyword>"` for full details. Most important rules (full versions in `rules-ui`):

- **Accessibility:** 4.5:1 contrast ratio, focus rings, alt text, aria-labels, keyboard nav, form labels
- **Touch:** 44x44px min targets, cursor-pointer on clickables, disable buttons during async
- **Performance:** WebP/srcset/lazy loading, `prefers-reduced-motion`, reserve space for async content
- **Layout:** `width=device-width`, 16px min body text, z-index scale (10,20,30,50), no horizontal scroll
- **Animation:** 150-300ms duration, use transform/opacity (not width/height), skeleton loaders
- **Icons:** SVG only (Heroicons/Lucide), no emojis as icons, consistent sizing (24x24 viewBox)

---

## Workflow

When user requests UI/UX work (design, build, create, implement, review, fix, improve), follow these steps:

### Step 0: Audit Existing Project Style (MANDATORY — SKIP ONLY IF THE PROJECT IS EMPTY)

**Before doing ANYTHING else, check if the project has existing UI code.**

#### 0.1 Scan for existing style definitions (in priority order)

```
1. design-system/MASTER.md (if exists — this IS the design system, USE IT)
2. tailwind.config.{js,ts,mjs} — colors, fontFamily, spacing, borderRadius, theme.extend
3. globals.css / global.css / index.css — CSS variables, @theme, @layer
4. UI component files — Button variants, Card styles, Typography, Spacing patterns
5. package.json — UI libraries (shadcn/ui, radix, chakra, mantine, etc.)
```

#### 0.2 Extract existing style

```
EXTRACTED STYLE:
- Colors: primary=#xxx, secondary=#xxx, background=#xxx, foreground=#xxx, muted=#xxx, accent=#xxx
- Typography: heading-font="xxx", body-font="xxx", heading-sizes=[...], body-size=xxx
- Spacing: base-unit=xxx (4px/8px?), common-gaps=[...]
- Border radius: sm=xxx, md=xxx, lg=xxx, full=xxx
- Shadows: card-shadow="xxx", button-shadow="xxx"
- Effects: glassmorphism? neumorphism? flat? gradients?
- Component patterns: button-variants=[...], card-style="..."
```

#### 0.3 Decision tree

```
IF design-system/MASTER.md exists:
    → USE IT AS SOURCE OF TRUTH. Skip --design-system generation.
ELSE IF clear style patterns in tailwind.config / CSS:
    → DO NOT run --design-system. Use extracted values. Search only missing pieces.
ELSE IF existing components show consistent style:
    → MATCH existing patterns exactly. Extend only what's missing.
ELSE (project is empty / no UI exists):
    → NOW use --design-system to create from scratch.
```

#### 0.4 What NEVER to do

- ❌ Ignore existing tailwind.config colors and use "recommended" palette
- ❌ Change existing border-radius patterns (e.g., project uses rounded-lg, you add rounded-full)
- ❌ Introduce new font families when project already has one
- ❌ Mix glassmorphism into a flat-design project
- ❌ Change existing shadow/lighting style
- ❌ Generate new design-system.md when one already exists

---

### Step 0b: Brief Inference (Read the Room)

Before touching code, **infer what the user actually wants**. Most LLM design output is bad because the model jumps to a default aesthetic instead of reading the room.

#### 0b.1 Read these signals first

1. **Page kind** — landing (SaaS / consumer / agency / event), portfolio (dev / designer / studio), redesign (preserve vs overhaul), editorial / blog, dashboard, form-heavy UI
2. **Vibe words** — "minimalist", "calm", "Linear-style", "Awwwards", "brutalist", "premium consumer", "Apple-y", "playful", "serious B2B", "editorial", "agency-y", "glassy", "dark tech"
3. **Reference signals** — URLs, screenshots, product names, competing brands
4. **Audience** — B2B procurement vs design-conscious consumer vs recruiter scanning a portfolio
5. **Brand assets** — logo, color, type, photography (for redesigns, these are starting material)
6. **Quiet constraints** — accessibility-first, public-sector, regulated industries, trust-first commerce

#### 0b.2 Output a one-line "Design Read" before generating

Before any code, state in one line: **"Reading this as: \<page kind> for \<audience>, with a \<vibe> language, leaning toward \<design system or aesthetic family>."**

Example reads:
- *"Reading this as: B2B SaaS landing for technical buyers, with a Linear-style minimalist language, leaning toward Tailwind utilities + Geist + restrained motion."*
- *"Reading this as: solo designer portfolio for hiring managers, with an editorial / kinetic-type language, leaning toward native CSS + scroll-driven animation + custom typography."*
- *"Reading this as: redesign of a public-sector service site, with a trust-first language, leaning toward GOV.UK Frontend or USWDS."*

#### 0b.3 If ambiguous, ask ONE question

Ask exactly **one** clarifying question — never a multi-question dump — and only when the design read genuinely diverges. If you can confidently infer from context, **do not ask**. Just declare the design read and proceed.

#### 0b.4 Anti-Default Discipline

Do not default to: AI-purple gradients, centered hero over dark mesh, three equal feature cards, generic glassmorphism on everything, infinite-loop micro-animations everywhere, Inter + slate-900. These are the LLM defaults. Reach past them deliberately based on the design read.

---

### Step 0c: The Three Dials

After the design read, set three dials. Every layout, motion, and density decision below is gated by these.

- **`DESIGN_VARIANCE: 8`** — 1 = Perfect Symmetry, 10 = Artsy Chaos
- **`MOTION_INTENSITY: 6`** — 1 = Static, 10 = Cinematic / Physics
- **`VISUAL_DENSITY: 4`** — 1 = Art Gallery / Airy, 10 = Cockpit / Packed Data

**Baseline:** `8 / 6 / 4`. Use these unless the design read overrides them.

#### Dial Inference (design read → dial values)

| Signal | VARIANCE | MOTION | DENSITY |
|---|---|---|---|
| "minimalist / clean / calm / editorial / Linear-style" | 5-6 | 3-4 | 2-3 |
| "premium consumer / Apple-y / luxury / brand" | 7-8 | 5-7 | 3-4 |
| "playful / wild / Dribbble / Awwwards / experimental / agency" | 9-10 | 8-10 | 3-4 |
| "landing page / portfolio / marketing site (default)" | 7-9 | 6-8 | 3-5 |
| "trust-first / public-sector / regulated / accessibility-critical" | 3-4 | 2-3 | 4-5 |
| "dashboard / data-heavy / admin panel" | 3-4 | 2-3 | 7-9 |
| "redesign - preserve" | match existing | +1 | match existing |
| "redesign - overhaul" | +2 | +2 | match existing |

#### Dial definitions (technical reference)

**DESIGN_VARIANCE (1-10):**
- 1-3: Symmetrical CSS Grid (12-col, equal fr-units), equal paddings, centered alignment
- 4-7: Overlapping elements, varied image aspect ratios, left-aligned headers over center-aligned data
- 8-10: Masonry layouts, fractional grid units, massive empty zones. MUST collapse to single-column on <768px

**MOTION_INTENSITY (1-10):**
- 1-3: No automatic animations. CSS `:hover`/`:active` only
- 4-7: CSS transitions with `cubic-bezier(0.16, 1, 0.3, 1)`. `animation-delay` cascades for load-ins
- 8-10: Complex scroll-triggered reveals, parallax, scroll-driven animation (GSAP ScrollTrigger or CSS `animation-timeline`). **NEVER use `window.addEventListener('scroll')`**

**VISUAL_DENSITY (1-10):**
- 1-3: Huge section gaps (`py-32` to `py-48`). Expensive, clean
- 4-7: Standard web app spacing (`py-16` to `py-24`)
- 8-10: Tight paddings. 1px lines separate data. `font-mono` for all numbers

---

### Step 0d: Design System Map (for greenfield projects)

When the project has no existing style, pick the right foundation:

| Brief reads as… | Reach for |
|---|---|
| Microsoft / enterprise SaaS / dashboards | `@fluentui/react-components` |
| Google-ish UI, Material-flavored | `@material/web` + Material 3 tokens |
| IBM-style B2B / enterprise analytics | `@carbon/react` + `@carbon/styles` |
| Public-sector UK service | `govuk-frontend` |
| US public-sector / trust-first | `uswds` |
| Modern accessible React foundation | `@radix-ui/themes` |
| Modern SaaS where you own the components | shadcn/ui (`npx shadcn@latest add ...`) |
| Tailwind-based modern SaaS / AI marketing | Tailwind v4 utilities + `dark:` variant |
| Fast local-business / agency MVP | Bootstrap 5.3 |

**One system per project.** Do not mix Fluent with Carbon in the same tree. Do not import shadcn/ui components into a Material 3 app.

**Honesty rule:** If the brief reads as one of the systems above, install and use the **official** package. Do not recreate its CSS by hand.

---

### Step 1: Analyze User Requirements

Extract: **Product type** (SaaS, e-commerce, portfolio, dashboard, etc.), **Style keywords** (minimal, playful, professional, etc.), **Industry** (healthcare, fintech, etc.), **Stack** (default: `html-tailwind`)

### Step 2: Generate Design System

**Only if Step 0 confirmed the project has NO existing style.** Otherwise, use extracted style.

```bash
uv run skills/design-ui-ux/scripts/search.py "<product_type> <industry> <keywords>" --design-system [-p "Project Name"]
```

Searches 5 domains in parallel (product, style, color, landing, typography), applies reasoning rules, returns complete design system with anti-patterns.

**Output formats:**
```bash
# ASCII box (default) - terminal display
uv run skills/design-ui-ux/scripts/search.py "fintech crypto" --design-system

# Markdown - best for documentation
uv run skills/design-ui-ux/scripts/search.py "fintech crypto" --design-system -f markdown
```

### Step 2b: Persist Design System (optional)

Add `--persist` to save for hierarchical retrieval across sessions:

```bash
uv run skills/design-ui-ux/scripts/search.py "<query>" --design-system --persist -p "Project Name"
# Creates: design-system/MASTER.md (global source of truth)
#          design-system/pages/    (page-specific overrides)

# With page override:
uv run skills/design-ui-ux/scripts/search.py "<query>" --design-system --persist -p "Project Name" --page "dashboard"
# Also creates: design-system/pages/dashboard.md
```

**Retrieval:** Check `design-system/pages/[page].md` first → override Master. If no page file, use `design-system/MASTER.md` exclusively.

**Context-aware retrieval prompt:**
```
I am building the [Page Name] page. Please read design-system/MASTER.md.
Also check if design-system/pages/[page-name].md exists.
If the page file exists, prioritize its rules.
If not, use the Master rules exclusively.
Now, generate the code...
```

### Step 3: Supplement with Detailed Searches

```bash
uv run skills/design-ui-ux/scripts/search.py "<keyword>" --domain <domain> [-n <max_results>]
```

| Domain | Use For |
|--------|---------|
| `product` | Product type recommendations (SaaS, e-commerce, healthcare, etc.) |
| `style` | UI styles and effects (glassmorphism, minimalism, brutalism, etc.) |
| `typography` | Font pairings, Google Fonts |
| `color` | Color palettes by product type |
| `landing` | Page structure, CTA strategies (hero, testimonial, pricing) |
| `chart` | Chart types and library recommendations |
| `ux` | Best practices, anti-patterns (accessibility, animation, z-index) |
| `react` | React/Next.js performance patterns |
| `web` | Web interface guidelines (aria, focus, semantic HTML) |
| `prompt` | AI prompts, CSS keywords for style generation |

### Step 3b: Generate UI Components with 21st.dev Magic (as needed)

```bash
bun skills/design-ui-ux/scripts/util_magic_ui_chat.ts <command> [options]
```

| Command | Use For |
|---------|--------|
| `21st-magic-component-builder` | Generate a UI component from description |
| `21st-magic-component-inspiration` | Get design inspiration |
| `21st-magic-component-refiner` | Refine an existing component |
| `logo-search` | Search for brand logos |

Run with `--help` on any command to see required flags.

### Step 4: Stack Guidelines (Default: html-tailwind)

```bash
uv run skills/design-ui-ux/scripts/search.py "<keyword>" --stack html-tailwind
```

Available stacks: `html-tailwind`, `react`, `nextjs`, `vue`, `svelte`, `swiftui`, `react-native`, `flutter`, `shadcn`, `jetpack-compose`

---

## Redesign Protocol

This skill handles greenfield builds AND redesigns. Misclassifying the mode is the single biggest source of bad redesign output.

### Detect the Mode

- **Greenfield** — no existing site, or full overhaul approved. Dial baseline from Section 0c
- **Redesign - Preserve** — modernise without breaking the brand. Audit first, extract brand tokens, evolve gradually
- **Redesign - Overhaul** — new visual language on existing content. Treat as greenfield for visuals; preserve content and IA

If ambiguous, ask **once**: *"Should this redesign preserve the existing brand, or are we starting visually from scratch?"*

### Audit Before Touching

Document the current state before proposing changes:
- **Brand tokens** — primary / accent colors, type stack, logo treatment, radii
- **Information architecture** — page tree, primary nav, key conversion paths
- **Content blocks** — what exists, what's doing work, what's filler
- **Patterns to preserve** — signature interactions, recognisable hero, copy voice
- **Patterns to retire** — AI-slop tells, broken layouts, dead links, generic stock
- **Dial reading of the existing site** — infer current dials. That's your starting point
- **SEO baseline** — current ranking pages, meta titles, structured data, OG cards. **SEO migration is the #1 redesign risk**

### Preservation Rules

- **Do not change information architecture** unless asked. Keep page slugs, anchor IDs, primary nav labels stable
- **Extract brand colors before applying color rules.** A brand that is already purple stays purple
- **Preserve copy voice** unless asked for a rewrite
- **Honor existing accessibility wins.** Do not regress focus states, alt text, keyboard nav, contrast
- **Respect existing analytics events.** Do not rename buttons, form fields, section IDs

### Modernisation Levers (priority order)

Apply in order — stop when the brief is satisfied:
1. Typography refresh — biggest visual lift per unit of risk
2. Spacing & rhythm — increase section padding, fix vertical rhythm
3. Color recalibration — desaturate, unify neutrals, keep brand accent
4. Motion layer — add `MOTION_INTENSITY`-appropriate micro-interactions
5. Hero & key-section recomposition
6. Full block replacement — only when existing block is unsalvageable

### What Never Changes Silently

- URL structure / route slugs
- Primary nav labels
- Form field names or order (breaks analytics + autofill)
- Brand logo or wordmark
- Existing legal / consent / cookie copy

---

## Icons & Fonts

### Icons

- **Allowed libraries (priority order):** `@phosphor-icons/react`, `hugeicons-react`, `@radix-ui/react-icons`, `@tabler/icons-react`, Heroicons, Lucide
- **NEVER hand-roll SVG icons.** If a glyph is missing, install a second library
- **One family per project.** Do not mix Phosphor with Lucide
- **Standardize `strokeWidth` globally** (e.g. `1.5` or `2.0`)

### Fonts

- Always use `next/font` (Next.js) or self-host with `@font-face` + `font-display: swap`. Never link Google Fonts via `<link>` in production

---

## Pre-Flight Check (MANDATORY)

Run this before delivering code. **If any box fails, the output is not done.**

> Detailed criteria for each checkbox live in `rules-ui` (AI Tells, Layout Discipline, Motion, Dark Mode, Performance & Accessibility).

### Brief & Dials
- [ ] Brief inference declared (one-line Design Read)?
- [ ] Dial values explicit and reasoned from the brief?
- [ ] Design system chosen appropriately?

### AI Tells (see `rules-ui` → AI Tells)
- [ ] **ZERO em-dashes (`—`) anywhere on the page?** Headlines, eyebrows, pills, body, quotes, attribution, captions, buttons, alt text. Zero. Non-negotiable.
- [ ] No Inter as default sans font (unless explicitly justified)?
- [ ] No AI-purple / blue glow gradients?
- [ ] No three equal feature cards?
- [ ] No generic names ("John Doe", "Acme", "Nexus")?
- [ ] No filler verbs ("Elevate", "Seamless", "Unleash")?
- [ ] No Fraunces or Instrument_Serif as serif defaults?

### Layout Discipline (see `rules-ui` → Layout Discipline)
- [ ] Hero fits viewport: headline <= 2 lines, subtext <= 20 words, CTA visible without scroll?
- [ ] Hero top padding max `pt-24`?
- [ ] Hero stack max 4 text elements? No tagline below CTAs, no trust strip in hero?
- [ ] EYEBROW COUNT: instances of `uppercase tracking` <= ceil(sectionCount / 3)?
- [ ] No split-header pattern (left headline + right small paragraph)?
- [ ] Zigzag alternation: no 3+ consecutive image+text-split sections?
- [ ] Section-Layout-Repetition: at least 4 different layout families across 8 sections?
- [ ] Navigation on ONE line at desktop, height <= 80px?
- [ ] Max 1 marquee per page?
- [ ] Mobile collapse explicit per section?

### Color & Shape (see `rules-ui` → Design Engineering Directives)
- [ ] Color Consistency Lock: one accent across all sections?
- [ ] Shape Consistency Lock: one corner-radius system?
- [ ] Premium-consumer palette NOT beige+brass+oxblood+espresso (if applicable)?
- [ ] Page Theme Lock: one theme for whole page?

### Buttons & Forms
- [ ] Button contrast: every CTA text readable against background (WCAG AA)?
- [ ] No CTA wraps to 2+ lines at desktop?
- [ ] No duplicate CTA intent on page?
- [ ] Form inputs, placeholders, focus rings, labels pass WCAG AA contrast?

### Typography
- [ ] Italic descender clearance: `leading-[1.1]` min + `pb-1` for words with `y g j p q`?

### Images & Assets
- [ ] Real images used (gen-tool, Picsum-seed, or explicit placeholders)? No div-based fake screenshots?
- [ ] Logo wall = logos only, no industry labels? Real SVG logos (Simple Icons / devicon)?
- [ ] No pills/labels overlaid on images?
- [ ] No photo-credit captions as decoration?
- [ ] Bento: at least 2-3 cells have real visual variation?

### Content
- [ ] Copy Self-Audit: no grammatically broken or AI-hallucinated phrases?
- [ ] No fake-precise numbers without real data or mock label?
- [ ] Quotes <= 3 lines body, clean attribution?

### Motion (see `rules-ui` → Motion & Animation)
- [ ] Every animation motivated (hierarchy / storytelling / feedback / state transition)?
- [ ] No `window.addEventListener('scroll')`?
- [ ] Reduced motion honored for everything `MOTION_INTENSITY > 3`?
- [ ] GSAP components have cleanup (`ctx.revert()`)?
- [ ] Motion isolated in client-leaf components with `'use client'`?

### Accessibility (see `rules-ui` → Performance & Accessibility)
- [ ] All images have alt text?
- [ ] Form inputs have labels?
- [ ] Color is not the only indicator?
- [ ] `prefers-reduced-motion` respected?
- [ ] Light mode text contrast 4.5:1 minimum?
- [ ] Glass/transparent elements visible in light mode?
- [ ] Borders visible in both modes?

### Performance (see `rules-ui` → Performance & Accessibility)
- [ ] Core Web Vitals plausible (LCP < 2.5s, INP < 200ms, CLS < 0.1)?
- [ ] `min-h-[100dvh]` instead of `h-screen`?
- [ ] Empty / loading / error states provided?
- [ ] One design system per project (no Material + shadcn mixed)?

### Banned patterns (see `rules-ui` → AI Tells)
- [ ] No version labels in hero? No section-number eyebrows?
- [ ] No decoration text strip at hero bottom?
- [ ] No scroll cues?
- [ ] No `border-t` + `border-b` on every row of long lists?
- [ ] No version footers on marketing pages?
- [ ] No micro-meta-sentences under eyebrows?
- [ ] No locale / city / time / weather strips (unless brief demands)?
- [ ] No decorative status dots (unless semantic state)?

If a single checkbox cannot be honestly ticked, the page is not done. Fix it before delivering.

---

## References

- `rules-ui` — Design Engineering Directives, Layout Discipline, Image & Visual Asset Strategy, AI Tells (forbidden patterns), Motion & Animation, Dark Mode Protocol, Performance & Accessibility, Common Pitfalls
- `rules-text-writing` — copywriting rules, anti-AI-cliché dictionary
