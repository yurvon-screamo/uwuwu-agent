# Verification with egui_kittest

Sources: egui_kittest 0.37 API, the foilcap runbook "UI-фиксы egui:
верификация на headless-ворктри фабрики", and the egui 0.37 experience
article in uwuwu.

## Harness basics

```rust
let mut harness = Harness::new_ui_state(
    |ui, enabled: &mut bool| { ui.checkbox(enabled, "Enable session"); },
    false,
);
harness.get_by_label("Enable session").click();
harness.run();
assert!(*harness.state());
```

Import `egui_kittest::kittest::Queryable` for the query methods. See
`examples/kittest_harness.rs`.

- Name every interactive widget. kittest selects by accessible name and panics
  when a widget has none.
- Check absence with `query_all_by_label_contains(...).next().is_none()`.
  `get_by_*` panics on a missing widget.
- Call `scroll_to_me()` before clicking a widget in a clipped `ScrollArea`.
- `interact()` on a click zone can override a button inside it. Prefer explicit
  named buttons.
- Build tests on relations: inside, wider than, changed. Do not assert values
  chosen by hand.

A harness crate needs `default_fonts` enabled on egui. Without it, a test
panics with "No glyph for ... in Proportional. Installed fonts: []". The
scratch check for this skill hit that failure and fixed it with
`egui = { version = "=0.37.0", features = ["default_fonts"] }`.

## Snapshots

- kittest writes `tests/snapshots/<name>.png`. A mismatch adds `.new.png` and
  `.diff.png`. Those artifacts are gitignored and do not appear in
  `git status`. Read the test output instead.
- Baselines depend on the render stack. Generate and commit them on one OS.
  Check whether the project's CI actually runs cargo tests; a snapshot gate
  may not exist.
- `UPDATE_SNAPSHOTS=true` can rewrite a baseline with an identical render.
  After an intended visual change, require a nonzero pixel delta. A zero delta
  is a red flag.
- A structural unit test is cheaper than a pixel diff for some contracts. For
  example, assert the effective text styles after `theme::apply()` instead of
  only looking at a screenshot.
- When two tasks touch the same PNG baselines, rebase on the fresh master and
  regenerate on the final tree. Do not hand-merge binary images.

## Accessibility tree with every screenshot

A screenshot alone does not prove that a control exists for assistive
technology. Read the AccessKit tree for the same window. A correct tree can
still reach screen readers incorrectly, so platform-level checks remain
necessary.

## Headless native runs

Factory worktrees have no window manager. There is no live fullscreen run
there. The deterministic layer is kittest. For a live check, build the debug
binary and run the real sidecar stack under Xvfb:

- unset Wayland: run with `-u WAYLAND_DISPLAY`. An inherited `WAYLAND_DISPLAY`
  sends winit to the real desktop instead of Xvfb.
- set `WGPU_BACKEND=gl`. With both backends available, eframe selects wgpu and
  headless Vulkan hangs on device enumeration.
- write INI config without quotes. The UI parser does not strip them.
- XTEST input can fail on modals; xlib or pyautogui injection can work where
  xdotool is absent. A fast series of synthetic drags loses the press. Use
  pauses between steps.

Report the boundary honestly: "no window manager, verified with kittest and a
1:1 screenshot". Do not claim a live fullscreen check.

## Checklist

- [ ] Widgets have visible names; no kittest panic on unnamed widgets.
- [ ] The failing test fails on the tree before the fix.
- [ ] Snapshot regeneration produced a nonzero delta after a visual change.
- [ ] `.new.png` and `.diff.png` were inspected, not only the exit code.
- [ ] The accessibility tree was read next to each screenshot.
- [ ] A live Xvfb run used `-u WAYLAND_DISPLAY` and `WGPU_BACKEND=gl`.
