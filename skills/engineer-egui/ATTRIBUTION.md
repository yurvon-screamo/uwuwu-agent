# Attribution

Provenance of every borrowed file, fragment and asset in this skill. Add a row
in the same change that adds the material. Keep the pinned revision or URL.

Code from storytold is MIT OR Apache-2.0. Their brand assets are not open
source and are not copied here. Fonts are SIL OFL 1.1. Standards from
storytold/craft-contrib are paraphrased with a link; that repository has no
license file at its root.

## Pinned sources

| Key | Source | Revision |
| --- | --- | --- |
| launcher | storytold/craft-launcher | `b7197ae7e5d22d1f3eeb8a27f62ed518654e7898` |
| contrib | storytold/craft-contrib | `646f558b8a9c0319faea6c938454634de5054864` |
| libs | storytold/craft-libs | main branch, read 2026-11-27 |
| foilcap | local vendored subset at `~/foilcap/ai-toolbox-ui` | read 2026-11-27 |

## Borrowed material

| Path | Source | License | Adaptation |
| --- | --- | --- | --- |
| `SKILL.md` | Experience/rust egui 0.37 article, ADR-028/030, foilcap task plan | uwuwu wiki | Hard rules and symptom routing restated. |
| `references/immediate-mode.md` | Experience/rust egui 0.37 article | uwuwu wiki | Frame lifecycle, identity and repaint rules condensed. |
| `references/layout-and-sizing.md` | ADR-028, ADR-030, foilcap task #1722 plan, local `framed()` helper in `~/foilcap/ai-toolbox-ui/src/app.rs`, egui 0.37 API | uwuwu wiki; helper is product code | Rules restated; `framed()` marked as local code, not an egui API. |
| `references/ecosystem.md` | docs.rs crate docs, emilk/egui README, Experience/rust egui 0.37 article, hafley66 ecosystem map (archived) | uwuwu wiki; docs | Crate map; community material used as a map only. |
| `references/theme-and-assets.md` | launcher `crates/ui-egui/src/theme.rs`; Projects/foilcap ADR-030 with full slug in the task body | MIT OR Apache-2.0 | Token names and values restated. Dark-only subset. |
| `references/upstream-storytold.md` | launcher, libs, contrib, craft-skills; ADR-030 | MIT OR Apache-2.0 for code; standards paraphrased | Repository map and exclusion list. No brand assets. |
| `references/accessibility.md` | contrib `standards/accessibility.md` | No root license; paraphrase with link | Rules shortened into an agent checklist. |
| `references/testing-kittest.md` | Projects/foilcap runbook "UI-фиксы egui: верификация на headless-ворктри фабрики"; Experience/rust article on egui 0.37 | uwuwu wiki | Procedures condensed; commands kept verbatim. |
| `references/gotchas.md` | ADR-028, ADR-030, Experience/rust article, foilcap tasks #1722 and #1732 | uwuwu wiki | Symptoms and mechanisms restated with source links. |
| `examples/theme_tokens.rs` | launcher `theme.rs` and foilcap `theme.rs` tokens | MIT OR Apache-2.0 | Rewritten as a minimal vanilla egui module. |
| `examples/responsive_row.rs` | foilcap task #1722 plan, egui 0.37 `Sides` and `egui_extras::StripBuilder` | MIT OR Apache-2.0 for the pattern | Rewritten with stable public APIs. |
| `examples/widget_button.rs` | launcher `widgets.rs` button pattern | MIT OR Apache-2.0 | Rewritten as a single native Button wrapper. |
| `examples/kittest_harness.rs` | egui_kittest 0.37 docs and kittest query API | Apache-2.0 | Minimal harness with named widgets. |
| `assets/fonts/OFL-Inter.txt` | Google Fonts, Inter | SIL OFL 1.1 | License text copied unmodified. |
| `assets/fonts/OFL-Archivo.txt` | Google Fonts, Archivo | SIL OFL 1.1 | License text copied unmodified. |
| `assets/fonts/OFL-GeistMono.txt` | Google Fonts, Geist Mono | SIL OFL 1.1 | License text copied unmodified. |
| `assets/fonts/manifest.json` | vendored font files in foilcap `ai-toolbox-ui` | SIL OFL 1.1 | URLs and SHA-256 hashes. No binaries stored. |
| `assets/palette.svg` | launcher `theme.rs` dark tokens | MIT OR Apache-2.0 | Swatch derived from token values. |
| `assets/README.md` | provenance rule from launcher `ATTRIBUTION.md` and ADR-030 | MIT OR Apache-2.0 | Asset-use procedure written for this skill. |

No snippet in this skill uses the storytold egui or winit fork API. No GPL,
LGPL or AGPL source was read.
