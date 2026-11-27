# Reverse of storytold

storytold (ArtCraft) is the primary source for craft UI patterns. Repositories
were read live on 2026-11-27. Pinned revisions are in `ATTRIBUTION.md`.

## Repository map

| Repository | What it carries | How to use it |
| --- | --- | --- |
| `storytold/craft-launcher` | `crates/ui-egui` with theme, widgets, dialogs, motion, icons, i18n and views; `ATTRIBUTION.md`; `docs/egui-037-migration-plan.md` | Main source for UI patterns and the attribution model. |
| `storytold/craft-libs` | Shared crates, `design/`, `design-lint`, cloud and colour UI crates | Map of the ecosystem. Read design intent; do not vendor code. |
| `storytold/craft-contrib` | `standards/accessibility.md`, `testing.md`, `clean-room.md`, `licences.md`, `naming.md`, `app-agents.md` | Rules for UI work, paraphrased with a link. No root license file. |
| `storytold/craft-skills` | README and agent instructions only | A note that the ready skill corpus is empty. No SKILL.md to copy. |
| `storytold/craft-fonts` | Font sources and licences | Upstream for font provenance. |
| `storytold/egui`, `storytold/winit` | Forks of the stack | Reference only. Never adopt the fork API. |

## What the vendored subset already contains

foilcap ADR-030 moved a subset of `ui-egui` into `ai-toolbox-ui`:

- `theme.rs`: named tokens, square corners, 1-point hairlines, craft text
  scale, font installation.
- `widgets.rs`: hud, tag, badge, rule, heading, button kinds, hairline cells,
  progress, glyphs and `human_bytes`.
- `dialogs.rs`: prompt frame, title and button-row patterns.
- `motion.rs`: reduce-motion application.

The vendored files are adapted to vanilla egui 0.37. Their `ATTRIBUTION.md`
row names the upstream file and the exact adaptations.

## What was left out and why

- The egui and winit forks. They use `ime_context`, `Style.right_to_left` and an
  extended `IMEOutput`. Adopting them would replace the whole stack.
- `craft-ui` and ICU4X. It does not build on vanilla egui 0.37, and the only
  needed piece was a `ComboBox`.
- i18n catalogues and the CJK system-font probe.
- Window chrome, launch-specific cards and split buttons.
- Brand files and app icons. Not open source.
- Light theme tokens. foilcap has a dark-only page lock.
- `craft-update` as a replacement for `self_update`. Out of scope.

## Rules for further reverse work

1. Verify the API against the pinned revision before copying a fragment.
2. Take only vanilla egui 0.37 code. If a fragment needs fork APIs, mark it
   `fragment` with the reason in `ATTRIBUTION.md`; do not paste it as a working
   example.
3. Copy licences with assets. Code is MIT OR Apache-2.0, fonts are OFL-1.1.
4. Do not read GPL, LGPL or AGPL sources. Do not copy wording from another
   product.
5. Put reusable widgets in this skill as examples, not into product code as a
   new dependency.

## Cross-project boundary

uwuwu#1734 audits review gates and agent instructions. It is a different
carrier from this skill. Keep the boundary.
