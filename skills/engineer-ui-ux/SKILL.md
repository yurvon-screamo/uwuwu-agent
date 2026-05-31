---
name: engineer-ui-ux
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

## Pre-Delivery Checklist

### Visual Quality
- [ ] No emojis as icons (SVG only)
- [ ] Icons from consistent set (Heroicons/Lucide)
- [ ] Brand logos verified from Simple Icons
- [ ] Hover states don't cause layout shift

### Interaction
- [ ] All clickable elements have `cursor-pointer`
- [ ] Hover states provide clear visual feedback
- [ ] Transitions 150-300ms
- [ ] Focus states visible for keyboard navigation

### Light/Dark Mode
- [ ] Light mode text contrast 4.5:1 minimum
- [ ] Glass/transparent elements visible in light mode
- [ ] Borders visible in both modes
- [ ] Test both modes before delivery

### Layout
- [ ] Floating elements have proper spacing from edges
- [ ] No content hidden behind fixed navbars
- [ ] Responsive at 375px, 768px, 1024px, 1440px
- [ ] No horizontal scroll on mobile

### Accessibility
- [ ] All images have alt text
- [ ] Form inputs have labels
- [ ] Color is not the only indicator
- [ ] `prefers-reduced-motion` respected
