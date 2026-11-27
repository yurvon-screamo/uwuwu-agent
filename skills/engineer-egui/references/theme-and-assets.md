# Theme tokens and assets

Sources: storytold/craft-launcher `crates/ui-egui/src/theme.rs` at revision
`b7197ae7e5d22d1f3eeb8a27f62ed518654e7898`, the vendored subset in foilcap
`ai-toolbox-ui/src/theme.rs`, and foilcap ADR-030.

## Design language

The craft language uses hard edges and hairlines. It does not use shadows or
rounded cards. One 1-point stroke separates rows.

| Role | Dark token |
| --- | --- |
| page and card surface | `#121316` |
| wells and row hover | `#060607` |
| body text | `#f2f1ee` |
| headings | `#ffffff` |
| secondary text | `#f2f1ee` at alpha 153 |
| tertiary text | `#f2f1ee` at alpha 97 |
| hairline | `#f2f1ee` at alpha 38 |
| stronger rule | `#f2f1ee` at alpha 102 |
| accent surface | `#2d81ff` |
| accent text | `#74aaff` |
| primary button | `#ffffff` on `#0b0b0c` |
| good / warn / bad | `#3fd6b8` / `#ffc14d` / `#ff6259` |

Widgets read a `Tokens` value. No widget hard-codes a colour. Write a
contract test that fails if a widget stops using tokens.

## Typography and fonts

- Archivo for display text and app names.
- Inter for body text.
- Geist Mono for buttons, labels and numbers.
- Use `FontTweak` and variation coordinates for variable fonts.
- Install fonts once per `egui::Context`. Repeated installs waste memory.

The fonts are SIL OFL 1.1. `assets/fonts/manifest.json` carries the upstream
URLs and SHA-256 hashes. Copy the matching `OFL-*.txt` with the font. Do not
download an unpinned version at build time.

## Emoji and coverage

egui's built-in fonts do not contain emoji. The launcher enables
`monochrome_emoji_fonts` for a fallback. Either enable that feature or use text
markers instead of emoji. eframe on web loads system fonts, so check CJK text
on macOS and Windows as well as in headless Chromium.

## Motion

Apply OS reduce-motion settings through the eframe host bridge. Set
`animation_time` to `0.0` and `scroll_animation` to none when the OS asks for
reduced motion. Keep progress and spinner motion: it reports ongoing work.

foilcap uses a constant reduce-motion flag. The OS signal wiring stays future
work. Do not claim a setting is honoured when a constant overrides it.

## Theme lock

ADR-030 locks foilcap to a dark-only theme. The upstream light tokens are not
vendored. If a project needs a light theme, take both token sets from the same
upstream file and test contrast in both themes.

## Asset provenance

The ArtCraft name, wordmark and "A" mark are trademarks. They are not MIT or
Apache-2.0. Do not copy brand files into a skill or a product. App icons and
nightly icon variants belong to their own repositories and keep their own
licences. Every copied asset gets an `ATTRIBUTION.md` row in the same change.

Source slugs in uwuwu: `Projects/foilcap/ADR/ADR-030: Дизайн-система
ai-toolbox-ui — vendored-subset от craft-launcher`.
