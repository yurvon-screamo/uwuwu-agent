---
name: rules-ui
description: Creating expressive, production-quality frontend interfaces. Use when the user asks to create web components, pages, artifacts, posters, or applications (websites, landing pages, dashboards, React components, HTML/CSS layouts, UI styling). Generates creative, polished code and design without typical AI aesthetics.
---

## Design Thinking

Before coding — understand the context and choose a BOLD aesthetic:
- **Essence**: What problem does the interface solve? Who is the user?
- **Tone**: Choose an extreme: brutal minimalism, maximalist chaos, retro-futurism, organic, luxury/refinement, toy-like, editorial/magazine, brutalism, art deco/geometry, soft/pastel, industrial, etc. Use as inspiration, but create your own unique style.
- **Constraints**: Technical requirements (framework, performance, accessibility).
- **Distinctiveness**: What will make the interface UNFORGETTABLE? What single detail will be remembered?

**CRITICAL**: Choose a clear direction and execute with precision. Bold maximalism and refined minimalism both work equally well — what matters is intentionality, not intensity.

## Frontend Aesthetics

Focus on:
- **Typography**: Choose beautiful, unique, interesting fonts. Avoid typical Arial and Inter — pick distinctive fonts that elevate the aesthetic. Pair: an expressive display font + a refined text font.
- **Color & Theme**: CSS variables for consistency. Dominant colors with sharp accents work better than pale uniform palettes.
- **Motion**: Animations for effects and micro-interactions. CSS-only for HTML. Motion library for React. One well-choreographed page load with a cascade of appearances (animation-delay) is better than a scatter of micro-animations. Scroll triggers and hover effects that surprise.
- **Space**: Unexpected layouts. Asymmetry. Overlaps. Diagonal flows. Breaking out of the grid. Generous negative space OR controlled density.
- **Backgrounds & Details**: Atmosphere and depth instead of flat colors. Gradient meshes, noise textures, geometric patterns, layered transparencies, dramatic shadows, decorative borders, custom cursors, grainy overlays.

NEVER use typical AI aesthetics: overused fonts (Inter, Roboto, Arial, system), cliché color schemes (purple gradients on white), predictable layouts, template design without character.

**IMPORTANT**: Implementation complexity should match the vision. Maximalism demands elaborate code with animations. Minimalism demands restraint and attention to spacing, typography, and subtle details.

## Accessibility

### Contrast

- Normal text: ≥ 4.5:1
- Large text (18px+): ≥ 3:1
- UI components: ≥ 3:1 against background

### Keyboard

- All interactive elements accessible via Tab
- Visible focus (outline/ring)
- Custom widgets: Enter to activate, Escape to close
- Modals trap focus, return focus on close

### Screen Readers

- All images have `alt` (or `alt=""` for decorative)
- All inputs associated with `<label>` or `aria-label`
- Buttons and links with descriptive text (not "Click here")
- One `<h1>` per page, headings without skipping levels
- Dynamic changes via `aria-live`

### Forms

- Visible label for every input
- Required fields marked (not only by color)
- Errors are specific and tied to the field

### Visual & Content

- Color is not the only means of conveying information
- Text scales to 200% without breaking layout
- Touch targets ≥ 44×44px on mobile

### ARIA Live Regions

| Value | Behavior | Use For |
|----------|-----------|----------|
| `aria-live="polite"` | Announce at next pause | Status, confirmations |
| `aria-live="assertive"` | Announce immediately | Errors, urgent alerts |
| `role="status"` | Like `polite` | Status messages |
| `role="alert"` | Like `assertive` | Error messages |

### Accessibility Anti-Patterns

| Anti-Pattern | Fix |
|---|---|
| `<div>` as button | `<button>` |
| No `alt` | Descriptive `alt` or `alt=""` |
| States by color only | Icons, text, patterns |
| Removed focus outlines | Style them, don't remove |
| Empty links/buttons | Text or `aria-label` |
| `tabindex > 0` | Only `0` or `-1` |
| Custom dropdown without ARIA | Native `<select>` or ARIA listbox |
