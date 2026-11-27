---
name: engineer-egui
description: "egui, eframe and egui_kittest engineering: immediate-mode state, layout and zoom, theme tokens, accessibility, snapshot testing and storytold/craft-launcher UI patterns. Use for egui UI code, scaling or layout bugs, kittest snapshots, theme and widget work."
---

# Engineer egui

Use this skill when you write or review Rust UI code built on egui, eframe,
egui_extras or egui_kittest. It carries the craft UI patterns reverse-engineered
from storytold and the failure cases recorded in the uwuwu wiki.

Read the crate versions first. The examples target **egui/eframe 0.37**, the
version used by foilcap and craft-launcher. If the project uses another version,
verify the API in docs.rs before reusing a snippet.

## Route by symptom

| Symptom or task | Read | Example |
| --- | --- | --- |
| New app, state, IDs, repaint, click lost on data arrival | `references/immediate-mode.md` | `examples/kittest_harness.rs` |
| Zoom, DPI, narrow panel, clipped buttons, table overflow | `references/layout-and-sizing.md` | `examples/responsive_row.rs` |
| Colours, fonts, hairlines, dark theme, icons, emoji | `references/theme-and-assets.md` | `examples/theme_tokens.rs` |
| Snapshot, kittest, headless run, screenshots | `references/testing-kittest.md` | `examples/kittest_harness.rs` |
| Keyboard, focus, screen reader, contrast | `references/accessibility.md` | `examples/widget_button.rs` |
| "Use a library or write our own?" | `references/ecosystem.md` | none |
| Odd failure with no obvious cause | `references/gotchas.md` | none |
| Reuse a storytold pattern | `references/upstream-storytold.md` | all examples |
| Fetch a font or read the palette | `assets/README.md` | `examples/theme_tokens.rs` |

## Hard rules

1. Keep state in the app struct. egui rebuilds the UI every frame. There is no
   retained widget tree.
2. Use `egui::Id::unique` for IDs that must stay stable across frames. `Id::new`
   is deprecated in 0.37.
3. Use native widgets for clickable controls. Do not paint a control with
   `Painter` and calculate its hit area by hand.
4. Read colours from theme tokens. Do not hard-code a colour in a widget.
5. Keep one module for width breakpoints and minimum sizes. A breakpoint is a
   number of **logical points**, not screen pixels.
6. Wrap a right-to-left row in `ui.horizontal` when it sits in a vertical
   parent. Otherwise it takes the parent's full height.
7. Give every interactive widget a name. kittest selects widgets by name and
   panics on unnamed widgets.
8. Check the pixel delta when you regenerate a snapshot. A zero delta after a
   visual change means the change did not reach the render.
9. Do not place a `ScrollArea` inside `egui::Modal`. That combination freezes
   input attribution in 0.37.
10. Call `Context::request_repaint` when bridge data arrives. egui sleeps when
    no input is pending, so a new widget may miss the click on its first frame.

## Verification loop

1. Name the target version and the target surface (native, wasm, headless).
2. Pick the matching reference. Take the example, not a guess.
3. Compile the example against the project's pinned dependencies before you
   adapt it. A fragment that does not compile on vanilla egui 0.37 is rewritten.
   The `fragment` status is reserved for code that really needs the storytold
   fork API. Record the reason in `ATTRIBUTION.md`.
4. Test the layout at 190, 300 and 400 points at 1, 2 and 3 pixels per point.
   Check that controls stay inside the window and clicks reach their targets.
5. For UI changes, read the accessibility tree next to every screenshot.

## Provenance

`ATTRIBUTION.md` lists every borrowed file, asset and fragment. Code from
storytold is MIT OR Apache-2.0. Fonts are OFL-1.1. Brand assets from storytold
are not open source and are never copied. All examples here use vanilla egui
APIs only.
