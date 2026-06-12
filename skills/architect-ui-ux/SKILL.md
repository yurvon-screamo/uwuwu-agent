---
name: architect-ui-ux
description: "UI/UX design guide."
---

⚠️ **CRITICAL RULE: ALWAYS START WITH AN AUDIT OF THE EXISTING PROJECT STYLE!**

**ALWAYS** use `DESIGN.md` if it exists in the project.

**NEVER generate a design system from scratch if the project already has UI.** Complete Step 0 below first.

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
[Detailed values for colors, typography, spacing, decoration]

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

Search `--domain ux "<keyword>"` for full details. Most important rules:

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

## Design Engineering Directives (Bias Correction)

LLMs default to clichés. Override these defaults proactively.

### Typography

- **Display / Headlines:** Default `text-4xl md:text-6xl tracking-tighter leading-none`
- **Body / Paragraphs:** Default `text-base text-gray-600 leading-relaxed max-w-[65ch]`
- **Sans font choice — discouraged as default:** `Inter`. Pick `Geist`, `Outfit`, `Cabinet Grotesk`, `Satoshi`, or a brand-appropriate serif first. Inter is acceptable when the user explicitly asks for a neutral / standard / Linear-style feel, or for accessibility-first / public-sector.
- **Pairings to know:** `Geist` + `Geist Mono`, `Satoshi` + `JetBrains Mono`, `Cabinet Grotesk` + `Inter Tight`

**Serif discipline (VERY DISCOURAGED as default):**
- Serif is only acceptable when the brand brief literally names a serif font, OR the aesthetic is genuinely editorial / luxury / publication / heritage
- **BANNED as defaults:** `Fraunces` and `Instrument_Serif` (the two LLM-favorite display serifs)
- If serif is justified (rare): rotate from PP Editorial New, GT Sectra Display, Recoleta, Cormorant Garamond, Playfair Display, EB Garamond, Canela — do NOT reuse the same serif across consecutive projects

**EMPHASIS RULE:** When you want to emphasize a word in a headline, use **italic or bold of the SAME font**. Do NOT inject a random serif word into a sans headline. Mixed-family emphasis is amateur.

**ITALIC DESCENDER CLEARANCE:** When italic is used in display type and the word contains a descender letter (`y g j p q`), `leading-[1]` will clip the descender. Use `leading-[1.1]` minimum and add `pb-1` or `mb-1` reserve.

### Color Calibration

- Max 1 accent color. Saturation < 80% by default
- **THE LILA RULE:** "AI Purple / Blue glow" aesthetic is discouraged as default. Use neutral bases (Zinc / Slate / Stone) with high-contrast singular accents (Emerald, Electric Blue, Deep Rose, Burnt Orange)
- **Override:** if the brand or brief explicitly asks for purple / violet — embrace it with intent
- **One palette per project.** Do not fluctuate between warm and cool grays
- **COLOR CONSISTENCY LOCK:** Once an accent color is chosen, it is used on the WHOLE page. A warm-grey site does not suddenly get a blue CTA in section 7

**PREMIUM-CONSUMER PALETTE BAN:**
- For premium-consumer briefs (cookware, wellness, artisan, luxury, DTC home goods) the LLM default is warm beige/cream + brass/clay/oxblood + espresso text. BANNED as default reach
- Default alternatives (rotate, do not reuse): Cold Luxury (silver-grey + chrome), Forest (deep green + bone + amber), Black and Tan, Cobalt + Cream, Terracotta + Slate, Pure monochrome + single saturated pop
- **Override:** beige+brass is acceptable ONLY when the brand brief explicitly names those colors

### Layout Diversification

- **ANTI-CENTER BIAS:** Centered Hero is avoided when `DESIGN_VARIANCE > 4`. Force split-screen, left-aligned content, asymmetric white-space, or scroll-pinned structures
- **Override:** centered hero is OK for editorial / manifesto / launch-announcement briefs

### Materiality, Shadows, Cards

- Use cards ONLY when elevation communicates real hierarchy. Otherwise group with `border-t`, `divide-y`, or negative space
- When a shadow is used, tint it to the background hue. No pure-black drop shadows on light backgrounds
- **SHAPE CONSISTENCY LOCK:** Pick ONE corner-radius scale for the page and stick to it. Mixed systems are allowed only with a documented rule followed everywhere

### Interactive UI States

Always implement full cycles:
- **Loading:** Skeletal loaders matching the final layout shape
- **Empty States:** Beautifully composed; indicate how to populate
- **Error States:** Clear, inline (forms), or contextual (toasts for transient)
- **Tactile Feedback:** On `:active`, use `-translate-y-[1px]` or `scale-[0.98]`
- **BUTTON CONTRAST CHECK:** Before shipping any button, verify button text is readable against button background. WCAG AA min (4.5:1 for body, 3:1 for large text 18px+)
- **CTA BUTTON WRAP BAN:** Button text MUST fit on one line at desktop. If a label wraps to 2+ lines, shorten it (3 words max for primary CTAs) or widen the button
- **NO DUPLICATE CTA INTENT:** Two CTAs with the same intent on one page = Fail. "Get in touch" + "Contact us" + "Let's talk" = all "contact" intent. Pick ONE label per intent

### Content Density

- **Default content shape per section:** short headline (<= 8 words) + short sub-paragraph (<= 25 words) + one visual asset OR one CTA
- **No data-dump sections.** Top 3-5 highlights + "View full list" link for long lists
- **Long lists need a different UI component.** If > 5 items, reach for: 2-column split, card grid, tabs/accordion, horizontal scroll-snap pills, carousel, or marquee
- **COPY SELF-AUDIT (mandatory before ship):** Re-read every visible string. Flag: grammatically broken, unclear referents, AI-hallucinated cute wordplay, forced metaphors. Rewrite every flagged string
- **Fake-precise numbers are banned** unless from real data or explicitly labeled as mock

---

## Layout Discipline (Hard Rules)

Failing any of these is shipping broken work.

### Hero Rules

- **Hero MUST fit in the initial viewport.** Headline max 2 lines on desktop, subtext max 20 words AND max 3-4 lines, CTAs visible without scroll
- **Hero font-scale discipline.** Default: `text-4xl md:text-5xl lg:text-6xl` for most heroes; `text-6xl md:text-7xl` only when headline is 3-5 words
- **HERO TOP PADDING CAP:** max `pt-24` at desktop. More = layout bug
- **HERO STACK DISCIPLINE (max 4 text elements):** 1) Eyebrow OR brand strip (pick zero or one), 2) Headline (max 2 lines), 3) Subtext (max 20 words), 4) CTAs (1 primary + max 1 secondary)
  - **BANNED in the hero:** tiny tagline below CTAs, trust micro-strip, pricing teaser, feature bullet list, social-proof avatar row
- **Hero needs a real visual.** Text + gradient blob is not a hero — it's a placeholder
- **"Used by" / "Trusted by" logo wall belongs UNDER the hero, never inside it**

### Navigation

- **Navigation MUST render on a single line** on desktop. If items don't fit at `lg` (1024px), condense labels or move to hamburger
- **Navigation height cap:** 80px max desktop, default 64-72px

### Bento & Grid

- **Bento grids MUST have rhythm.** Vary composition: alternate full-width feature rows, asymmetric tile sizes, vertical breaks
- **Bento cell count rule:** N items = N cells. If your grid has an empty cell, re-shape the grid
- **Bento background diversity:** At least 2-3 cells need real visual variation (image, gradient, pattern, tinted background). Not all white-on-white text cards

### Section Variety

- **Section-Layout-Repetition Ban.** Once you use a layout family for a section, it can appear at most ONCE on the page. A landing page with 8 sections must use at least 4 different layout families
- **ZIGZAG ALTERNATION CAP:** Max 2 consecutive sections with image+text-split pattern. The 3rd = Fail. Break with full-width, vertical-stack, bento, or marquee

### Eyebrow Restraint

- **Maximum 1 eyebrow per 3 sections.** Hero counts as 1
- If section A has an eyebrow, the next 2 sections cannot have one
- **Pre-Flight Check is mechanical:** count instances of `uppercase tracking` across all section components. If count > ceil(sectionCount / 3), output fails
- **What to do instead:** drop it entirely. The headline alone is enough

### Split-Header Ban

The pattern "left big headline + right small explainer paragraph" as a section header is **banned as default**. If you need both a headline and an explainer, stack them vertically (headline on top, body below, max-width 65ch).

### Other

- **Mobile collapse must be explicit per section.** For every multi-column layout, declare the `< 768px` fallback
- **Viewport stability:** NEVER use `h-screen` for full-height sections. ALWAYS use `min-h-[100dvh]`
- **Grid over Flex-Math:** NEVER use complex flexbox percentage math. ALWAYS use CSS Grid
- **Max 1 marquee per page**

---

## Image & Visual Asset Strategy

Landing pages and portfolios are **visual products**. Text-only pages with fake-screenshot divs are slop.

**Priority order for visual assets:**

1. **Image-generation tool first.** If ANY image-gen tool is available, use it to create section-specific assets
2. **Real web images second.** Acceptable defaults:
   - `https://picsum.photos/seed/{descriptive-seed}/{w}/{h}` for placeholder photography
   - Actual stock or brand URLs from the brief
3. **Last resort:** leave clearly-labeled placeholder slots (`<!-- TODO: hero photo, 1600x1200 -->`) and tell the user

**Even minimalist sites need real images.** A pure-text page is not minimalism. It is incomplete work.

**Real company logos for social proof.** Use real SVG logos from Simple Icons (`https://cdn.simpleicons.org/{slug}/ffffff`). For invented brands, generate a simple monogram SVG. Plain text wordmarks look generic.

**LOGO-ONLY rule:** logo wall = logos and nothing else. Do NOT print industry / category labels below each logo.

**Div-based fake screenshots are banned.** A "product preview" built from styled `<div>` rectangles is the #1 LLM-design Tell.

---

## AI Tells (Forbidden Patterns)

Avoid these signatures unless the brief explicitly asks for them.

### Visual & CSS

- **NO neon / outer glows** by default. Use inner borders or subtle tinted shadows
- **NO pure black (`#000000`).** Off-black, zinc-950, or charcoal
- **NO oversaturated accents.** Desaturate to blend with neutrals
- **NO excessive gradient text** for large headers
- **NO custom mouse cursors.** Accessibility-hostile, perf-hostile
- **NO three-column equal feature cards.** The generic "three identical cards" row is banned

### Typography

- **AVOID Inter as default.** See Design Engineering > Typography section
- **NO oversized H1s** that just scream. Control hierarchy with weight + color, not raw scale

### Content & Data ("Jane Doe" Effect)

- **NO generic names.** "John Doe", "Sarah Chan" → use creative, realistic, locale-appropriate names
- **NO generic avatars.** No SVG "egg" or user icons → use believable photo placeholders
- **NO fake-perfect numbers.** Avoid `99.99%`, `50%`, `1234567`. Use organic data (`47.2%`, `+1 (312) 847-1928`)
- **NO startup-slop brand names.** "Acme", "Nexus", "SmartFlow", "Cloudly" → invent contextual, premium names
- **NO filler verbs.** "Elevate", "Seamless", "Unleash", "Next-Gen", "Revolutionize" → concrete verbs only

### Production-Test Tells (banned outright)

**Hero & top-of-page:**
- NO version labels in hero (`V0.6`, `BETA`, `INVITE-ONLY`) unless brief is a product launch
- NO "Brand · No. 01"-style sub-eyebrows

**Section numbering & labels:**
- NO section-number eyebrows (`00 / INDEX`, `001 · Capabilities`, `06 · how it works`)
- NO `01 / 4`-style pagination on images or bento tiles
- NO "Index of Work, 2018 - 2026"-style range labels as eyebrows

**Separators & dots:**
- Middle-dot (`·`) is rationed. Maximum 1 per line in metadata strips
- NO decorative colored status dots on every list/nav/badge. Only for real semantic state

**Em-dashes:**
- **EM-DASH (`—`) IS COMPLETELY BANNED.** In headlines, eyebrows, pills, body, quotes, attribution, captions, buttons, alt text. Zero. Use hyphen (`-`), period, comma, or parentheses
- En-dash (`–`) as separator is also banned. Date ranges use hyphen (`2018-2026`)

**Typography flourishes:**
- NO `<br>`-broken-and-italicized headlines as a default "design move"
- NO vertical rotated text unless brief is explicitly agency / Awwwards / experimental
- NO crosshair / hairline grid lines as decoration. Only when they organize real content

**Fake product previews:**
- NO div-based fake product UI in the hero (fake task lists, dashboards, terminals)
- NO fake version footers inside fake screenshots ("v0.6.2-rc.1", "last sync 4s ago · main")

**Marketing-copy Tells:**
- NO "Quietly in use at" / "Quietly trusted by" social-proof headers
- NO "From the field" / "Field notes" / "Currently on the bench" poetic labels
- NO weather / locale strips ("LIS 14:23 · 18°C") unless brief is about a place
- NO micro-meta-sentences under eyebrows
- NO generic step labels ("Stage 1 / Stage 2 / Stage 3"). Use verb-noun directly ("Install", "Configure", "Ship")

**Pills, labels, decoration:**
- NO pills/labels overlaid on images
- NO photo-credit captions as decoration (`Field study no. 12 · Ines Caetano`)
- NO version footers on marketing pages (`v1.4.2`, `Build 0048`)
- NO decoration text strip at hero bottom (`BRAND. MOTION. SPATIAL.`)
- NO floating top-right sub-text in section headings
- NO scroll cues (`Scroll`, `↓ scroll`, `Scroll to explore`)

**Lists & dividers:**
- NO `border-t` + `border-b` on every row of long lists. Pick one, use sparsely
- NO scoring/progress bars with filled background tracks on landing pages

---

## Motion & Animation

### When to use motion

- Motion is context-aware, not automatic. Use when `MOTION_INTENSITY > 4` AND the section benefits
- **MOTION MUST BE MOTIVATED:** Before adding any animation, ask "what does this communicate?" Valid: hierarchy, storytelling, feedback, state transition. Invalid: "it looked cool"
- **Motion claimed = motion shown:** If `MOTION_INTENSITY > 4`, the page must actually animate. If you cannot ship working motion, drop the dial to 3

### Stack conventions

- **Animation library:** Motion (`motion/react`). Import from `motion/react`, not `framer-motion`
- **GSAP + ScrollTrigger:** for full-page scrolltelling and scroll hijacks. Isolate in dedicated leaf components with `useEffect` cleanup
- **NEVER mix GSAP / Three.js with Motion in the same component tree**

### State management for motion

- **NEVER** use `useState` for continuous values (mouse position, scroll progress, pointer physics). Use Motion's `useMotionValue` / `useTransform` / `useScroll`
- Local `useState` / `useReducer` for isolated UI state only

### Reduced Motion (mandatory)

- Any motion above `MOTION_INTENSITY > 3` MUST honor `prefers-reduced-motion`
- In Motion: wrap with `useReducedMotion()` and degrade to static
- In CSS: gate behind `@media (prefers-reduced-motion: no-preference)`

### Forbidden animation patterns

- **`window.addEventListener("scroll", ...)`** — banned. Use Motion's `useScroll()`, GSAP ScrollTrigger, IntersectionObserver, or CSS scroll-driven animations
- **Custom scroll progress using `window.scrollY` in React state** — same reason
- **`requestAnimationFrame` loops touching React state** — use motion values instead

### Canonical: Scroll-Reveal Stagger (lighter alternative)

For "items appear as they enter viewport" (no pinning), prefer Motion's `whileInView`:

```tsx
"use client";
import { motion, useReducedMotion } from "motion/react";

export function RevealStagger({ items }: { items: string[] }) {
  const reduce = useReducedMotion();
  return (
    <ul className="grid gap-6">
      {items.map((item, i) => (
        <motion.li
          key={item}
          initial={reduce ? false : { opacity: 0, y: 24 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, amount: 0.3 }}
          transition={{
            duration: 0.6,
            delay: i * 0.06,
            ease: [0.16, 1, 0.3, 1],
          }}
        >
          {item}
        </motion.li>
      ))}
    </ul>
  );
}
```

### Canonical: GSAP Sticky-Stack

```tsx
"use client";
import { useRef, useEffect } from "react";
import { gsap } from "gsap";
import { ScrollTrigger } from "gsap/ScrollTrigger";
import { useReducedMotion } from "motion/react";

gsap.registerPlugin(ScrollTrigger);

export function StickyStack({ cards }: { cards: React.ReactNode[] }) {
  const ref = useRef<HTMLDivElement>(null);
  const reduce = useReducedMotion();

  useEffect(() => {
    if (reduce || !ref.current) return;
    const ctx = gsap.context(() => {
      const cardEls = gsap.utils.toArray<HTMLElement>(".stack-card");
      cardEls.forEach((card, i) => {
        if (i === cardEls.length - 1) return;
        ScrollTrigger.create({
          trigger: card,
          start: "top top",
          endTrigger: cardEls[cardEls.length - 1],
          end: "top top",
          pin: true,
          pinSpacing: false,
        });
        gsap.to(card, {
          scale: 0.92,
          opacity: 0.55,
          ease: "none",
          scrollTrigger: {
            trigger: cardEls[i + 1],
            start: "top bottom",
            end: "top top",
            scrub: true,
          },
        });
      });
    }, ref);
    return () => ctx.revert();
  }, [reduce]);

  return (
    <div ref={ref} className="relative">
      {cards.map((card, i) => (
        <div
          key={i}
          className="stack-card sticky top-0 min-h-[100dvh] flex items-center justify-center"
        >
          {card}
        </div>
      ))}
    </div>
  );
}
```

Critical: `start: "top top"`, `pin: true`, every card except last is pinned, scale/opacity driven by NEXT card's scroll trigger.

### Canonical: GSAP Horizontal-Pan

```tsx
"use client";
import { useRef, useEffect } from "react";
import { gsap } from "gsap";
import { ScrollTrigger } from "gsap/ScrollTrigger";
import { useReducedMotion } from "motion/react";

gsap.registerPlugin(ScrollTrigger);

export function HorizontalPan({ children }: { children: React.ReactNode }) {
  const wrap = useRef<HTMLDivElement>(null);
  const track = useRef<HTMLDivElement>(null);
  const reduce = useReducedMotion();

  useEffect(() => {
    if (reduce || !wrap.current || !track.current) return;
    const ctx = gsap.context(() => {
      const distance = track.current!.scrollWidth - window.innerWidth;
      gsap.to(track.current, {
        x: -distance,
        ease: "none",
        scrollTrigger: {
          trigger: wrap.current,
          start: "top top",
          end: () => `+=${distance}`,
          pin: true,
          scrub: 1,
          invalidateOnRefresh: true,
        },
      });
    }, wrap);
    return () => ctx.revert();
  }, [reduce]);

  return (
    <section ref={wrap} className="relative overflow-hidden">
      <div ref={track} className="flex h-[100dvh] items-center">
        {children}
      </div>
    </section>
  );
}
```

Critical: `start: "top top"`, `pin: true`, `end: "+=${distance}"`, `scrub: 1`.

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

## Common Pitfalls to Avoid

| Area | Do | Don't |
|------|----|----- |
| **Icons** | SVG icons (Heroicons, Lucide, Simple Icons) | Emojis as UI icons |
| **Hover** | Color/opacity transitions | Scale transforms that shift layout |
| **Cursor** | `cursor-pointer` on all clickable elements | Default cursor on interactive elements |
| **Glass light mode** | `bg-white/80` or higher | `bg-white/10` (invisible) |
| **Text contrast light** | `#0F172A` (slate-900) for text | `#94A3B8` (slate-400) for body |
| **Muted text light** | `#475569` (slate-600) minimum | gray-400 or lighter |
| **Borders light mode** | `border-gray-200` | `border-white/10` (invisible) |
| **Floating navbar** | `top-4 left-4 right-4` spacing | Stick to `top-0 left-0 right-0` |
| **Content padding** | Account for fixed navbar height | Let content hide behind fixed elements |
| **Max-width** | Same `max-w-6xl` or `max-w-7xl` throughout | Mix different container widths |
| **Theme colors** | Use directly (`bg-primary`) | Wrap in `var()` |
| **Transitions** | `transition-colors duration-200` | Instant changes or >500ms |

---

## Dark Mode Protocol

Dual-mode by default. Never assume light-only unless the brief is print-emulating editorial.

- Use Tailwind `dark:` variant OR CSS variables for tokens. Pick one strategy per project
- Maintain visual hierarchy, brand identity, and WCAG AA contrast across both modes
- Respect `prefers-color-scheme: dark`. Default to system preference unless brand insists
- **Page Theme Lock:** ONE theme (light, dark, or auto) for the whole page. Sections do not invert. Exception: one deliberate theme switch with strong transition
- No pure `#000000` and no pure `#ffffff` — use off-black and off-white
- Test in both modes before finishing

---

## Performance & Accessibility

### Hardware Acceleration
- Animate ONLY `transform` and `opacity`. Never animate `top`, `left`, `width`, `height`
- Use `will-change: transform` sparingly

### Core Web Vitals Targets
- **LCP** < 2.5s. Hero image must be `next/image priority` or preloaded
- **INP** < 200ms. Heavy work off main thread
- **CLS** < 0.1. Reserve space for images, fonts, embeds

### Z-Index Restraint
- Use z-index strictly for systemic layer contexts (sticky navbars, modals, overlays, grain)
- Document the z-index scale in a project constants file

---

## Pre-Flight Check (MANDATORY)

Run this before delivering code. **If any box fails, the output is not done.**

### Brief & Dials
- [ ] Brief inference declared (one-line Design Read)?
- [ ] Dial values explicit and reasoned from the brief?
- [ ] Design system chosen appropriately?

### AI Tells
- [ ] **ZERO em-dashes (`—`) anywhere on the page?** Headlines, eyebrows, pills, body, quotes, attribution, captions, buttons, alt text. Zero. Non-negotiable.
- [ ] No Inter as default sans font (unless explicitly justified)?
- [ ] No AI-purple / blue glow gradients?
- [ ] No three equal feature cards?
- [ ] No generic names ("John Doe", "Acme", "Nexus")?
- [ ] No filler verbs ("Elevate", "Seamless", "Unleash")?
- [ ] No Fraunces or Instrument_Serif as serif defaults?

### Layout Discipline
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

### Color & Shape
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

### Motion
- [ ] Every animation motivated (hierarchy / storytelling / feedback / state transition)?
- [ ] No `window.addEventListener('scroll')`?
- [ ] Reduced motion honored for everything `MOTION_INTENSITY > 3`?
- [ ] GSAP components have cleanup (`ctx.revert()`)?
- [ ] Motion isolated in client-leaf components with `'use client'`?

### Accessibility
- [ ] All images have alt text?
- [ ] Form inputs have labels?
- [ ] Color is not the only indicator?
- [ ] `prefers-reduced-motion` respected?
- [ ] Light mode text contrast 4.5:1 minimum?
- [ ] Glass/transparent elements visible in light mode?
- [ ] Borders visible in both modes?

### Performance
- [ ] Core Web Vitals plausible (LCP < 2.5s, INP < 200ms, CLS < 0.1)?
- [ ] `min-h-[100dvh]` instead of `h-screen`?
- [ ] Empty / loading / error states provided?
- [ ] One design system per project (no Material + shadcn mixed)?

### Banned patterns
- [ ] No version labels in hero? No section-number eyebrows?
- [ ] No decoration text strip at hero bottom?
- [ ] No scroll cues?
- [ ] No `border-t` + `border-b` on every row of long lists?
- [ ] No version footers on marketing pages?
- [ ] No micro-meta-sentences under eyebrows?
- [ ] No locale / city / time / weather strips (unless brief demands)?
- [ ] No decorative status dots (unless semantic state)?

If a single checkbox cannot be honestly ticked, the page is not done. Fix it before delivering.
