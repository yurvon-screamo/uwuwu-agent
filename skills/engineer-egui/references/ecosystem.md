# egui ecosystem map

Read `Cargo.toml` before using any crate. Versions below were the versions in
the foilcap and craft-launcher stacks on 2026-11-27. Verify the API in docs.rs
for the pinned version. A skill does not add a dependency to a project.

| Crate | Use it for | Watch out |
| --- | --- | --- |
| `egui` 0.37 | Immediate-mode widgets, layout, `Painter`, `Id` | `Id::new` is deprecated; use `Id::unique`. |
| `eframe` 0.37 | Native and web application shell | Web pulls system fonts; native targets need a check. |
| `egui_extras` 0.37 | `StripBuilder`, `Table`, image loaders, syntax highlight | `StripBuilder::clip(true)` for narrow cells. |
| `egui_kittest` 0.37 | Harness, named queries, snapshots | Needs `default_fonts` on egui; snapshots depend on the render stack. |
| `egui_commonmark` 0.26 | Markdown rendering | Supply a `source_id` unique per viewer and stable across frames. Table overflow needs app-level handling (foilcap ADR-028). |
| `egui_table` | Virtual-scrolling tables with sticky columns | Only when a plain `Grid` cannot hold the data volume. |
| `egui_dock` | IDE-style dockable panels | Adds a retained layout state model. |
| `egui_flex` | Flexbox-style layout | Check that native `Sides` and `StripBuilder` cannot express the layout first. |
| `egui_overlay` | Transparent click-through desktop overlays | Separate product shape; macOS alpha and click-through need their own checks. |
| `accesskit` | Accessibility tree behind egui | Prefer egui's widget names and roles before custom nodes. |

## Rules for choosing

1. Use the native widget first. A new crate must remove a proven limitation.
2. Check the licence before adding a dependency. The uwuwu Design Spec keeps a
   stack allow-list; a new library outside the frame needs the owner's
   agreement.
3. Keep UI and protocol crates free of server-only dependencies. reqwest and
   tokio do not build for wasm32. Keep shared types serde-only.
4. Pin wasm-bindgen where the project pins it. Version 0.2.130 failed with
   rustc 1.98 in the observed stack; 0.2.128 / 0.3.105 worked.

Sources: docs.rs for each crate, emilk/egui README and examples, the egui 0.37
experience article in uwuwu, and the hafley66/claude-research egui ecosystem
map (archived, used as a map only).
