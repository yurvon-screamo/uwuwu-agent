# Gotchas from real incidents

Each row names the symptom, the mechanism and the source. Verify the symptom in
the project before you apply the fix.

| Symptom | Mechanism | Source |
| --- | --- | --- |
| Snapshot fails only on another machine | kittest snapshots depend on the wgpu render stack. Generate and commit baselines on one OS. | egui 0.37 experience article |
| kittest panics on an unnamed widget | AccessKit selectors are the test handles. Name every interactive widget. | egui 0.37 experience article |
| Bottom panel empty in a harness | Panel API under eframe + kittest. Use an explicit block in `CentralPanel`. | egui 0.37 experience article |
| Several markdown answers share one viewer cache | `egui_commonmark` 0.26 needs a `source_id` stable across frames and unique per viewer. Use `Id::unique(("answer", index))`. | egui 0.37 experience article |
| wasm build fails with "failed to find intrinsics" | wasm-bindgen 0.2.130 breaks rustc 1.98. Pin 0.2.128 / 0.3.105 until upstream fixes it. | egui 0.37 experience article |
| wasm target pulls a server crate | UI and protocol crates must not depend on core crates that use reqwest or tokio. Keep those types serde-only. | egui 0.37 experience article |
| Emoji render as empty boxes | egui built-in fonts have no emoji. Enable `monochrome_emoji_fonts` or use text markers. | egui 0.37 experience article |
| CJK differs between platforms | eframe on web pulls system fonts. Check macOS and Windows with a real run. | egui 0.37 experience article |
| IME works on one platform only | IME fixes are version and platform specific. Decide on the target host, not by bumping a version. | egui 0.37 experience article |
| Side panel springs back after drag | `PanelState` stores the content `min_rect` on non-drag frames. See `layout-and-sizing.md`. | egui 0.37 experience article |
| Right-to-left row fills the panel height | `with_layout` allocates the full remaining rect. Wrap it in `ui.horizontal`. | egui 0.37 experience article |
| Snapshot artifacts "not in git status" | `.new.png` and `.diff.png` are gitignored. Read the test output or open the file directly. | egui 0.37 experience article |
| Click lands on a hovered row but does nothing | Hit-test uses the previous frame, and bridge data did not request a repaint. | egui 0.37 experience article |
| Modal ignores mouse and Esc | A `ScrollArea` inside `egui::Modal` freezes input attribution. Keep modal content flat. | egui 0.37 experience article |
| Synthetic mouse cannot click a modal | XTEST and Modal interaction is environment dependent. Native kittest input still works. Confirm with a real mouse at acceptance. | foilcap UI-fix runbook |
| A fast series of synthetic drags is lost | egui misses the press when drags have no pause. Use human-speed steps. | foilcap UI-fix runbook |
| Screener flags hairline borders as nested cards | On the craft hairline language, a p0.62 border flag on both old and new frames is noise. Check the token discipline and the pixel baseline before rewriting. | foilcap UI-fix runbook |
| Parallel tasks produce a binary PNG conflict | Two tasks regenerate the same baseline. Rebase, then regenerate on the final tree. Never hand-merge a PNG. | foilcap UI-fix runbook |
| Snapshot regeneration is green with no visual change | `UPDATE_SNAPSHOTS=true` can overwrite a baseline with the same render. Require a nonzero pixel delta after an intended visual change. | foilcap UI-fix runbook |
| Layout collapses when zoom rises | Points shrink while font size stays. Use the 190/300/400 point matrix and one breakpoint module. | foilcap task #1722 |
| Custom painter controls escape the panel | Absolute offsets ignore the available rect. Use `Sides`, `StripBuilder` and `Widget`. | foilcap tasks #1722 and #1732 |

All wiki slugs are recorded in `ATTRIBUTION.md` and in the task body of
uwuwu#1733.
