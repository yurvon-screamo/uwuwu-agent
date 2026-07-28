---
name: rules-ui
description: "UI implementation rules — design engineering directives, layout discipline, image strategy, AI tells (forbidden patterns), motion, dark mode, performance & accessibility, common pitfalls. Apply when writing UI code."
---

# UI Implementation Rules

> Strategy, workflow, redesign protocol, and pre-flight checklist — in `architect-ui-ux`. This skill is the **rules layer**: what to do and what to avoid when implementing UI.

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

### Core Web Vitals

See `rules-performance` for the full Core Web Vitals targets (LCP/INP/CLS), optimization process, and frontend/backend performance rules. For UI specifically: hero images must be `next/image priority` or preloaded; reserve space for images, fonts, and embeds to prevent CLS.

### Z-Index Restraint
- Use z-index strictly for systemic layer contexts (sticky navbars, modals, overlays, grain)
- Document the z-index scale in a project constants file

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
