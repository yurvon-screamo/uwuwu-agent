# Layout, sizing and DPI

Sources: egui 0.37 `Sides` and `egui_extras` 0.37 `StripBuilder`, foilcap
ADR-028 and ADR-030, the plan of foilcap task #1722, and the egui 0.37
experience article in uwuwu.

## Points are not pixels

egui lays out in logical points. `pixels_per_point` is 2.0 on a retina display.
When the VS Code zoom rises, the panel gets fewer points while the font keeps
its size. Layout constants must be points. Derive them in one module and use
that module everywhere.

Acceptance matrix for a layout change: 190, 300 and 400 points wide at 1, 2 and
3 pixels per point. At every combination:

- every control stays inside the window;
- clicks reach their targets;
- no text is clipped or painted over another control;
- the primary task still completes (for example, create a session).

## Use native primitives

| Need | Use | Avoid |
| --- | --- | --- |
| Header with a trailing action | `egui::Sides::new().show(...)` with `shrink_right()` and `truncate()` | absolute painter offsets |
| Narrow column next to a flexible one | `egui_extras::StripBuilder` with `Size::remainder().at_least(min)` and `clip(true)` | hand-computed rectangles |
| Custom control | `impl egui::Widget` for a small struct | painter glyph plus manual hit test |
| Aligned data | `egui::Grid` | nested `horizontal` stacks |
| Long content | `ScrollArea` with an explicit `max_height` | unbounded vertical stack |

`Sides::truncate()` shows the start of a long title and adds an ellipsis. It
keeps the trailing button inside the available width.

`Layout::right_to_left` used directly inside a top-down parent allocates the
full remaining rectangle, including its height. One row then sits in the
vertical centre of the panel. Wrap the `with_layout` call in `ui.horizontal` to
limit the row to its content height. Removing that wrapper is a visual
regression, not a simplification.

## Panel width spring

egui 0.37 stores a `SidePanel` width in `PanelState` every frame that is not a
drag frame. It writes the frame's `min_rect`. If a row is wider than the panel,
the content width wins and the panel springs back after every drag.

Fix both parts:

1. Render rows right-to-left: measure the trailing button first, pin it to the
   right edge, then let the title `truncate()` in the remaining space.
2. Set `ScrollArea::auto_shrink([false, false])` so content cannot define the
   panel width.

Test the fix with content that overflows. A fixture without overflow passes
even on broken code.

## Degenerate scroll viewport

A table inside a markdown stream gets a viewport with zero height when the
parent allocates no height. `ScrollArea::horizontal` then receives no pointer
containment, and its content is clipped. `egui::Frame::framed()` has the same
effect on viewport-dependent widgets: it can give the stream about 20 points
of height.

Fix by allocating an explicit height from the previous frame. Store the
measured height in `Memory` with `insert_temp` and read it with `get_temp`; the
layout converges on the second frame. Use `ScrollStyle::thin()` globally so
bars are visible when there is something to scroll. Set table cells to
`TextWrapMode::Extend` so they do not wrap.

## Scroll and modal

- Before clicking a widget inside a fixed-height `ScrollArea` in a harness, call
  `harness.get_by_label(...).scroll_to_me()`. A clipped widget cannot receive
  the click.
- A `Modal` with an internal `ScrollArea` stops receiving hover, click and Esc
  attribution in 0.37. Keep modal content free of internal `ScrollArea`.
- Check widget text with `query_all_by_label_contains(...).next().is_none()`.
  The `get_by_*` variants panic when the widget is absent.

## Red lines

- No duplicated width literals in several files.
- No painter geometry for clickable controls.
- No assumption that a passing snapshot at 1000 points covers a narrow panel.
- No manual merge of PNG baselines. Regenerate on the final tree after the
  competing change is merged.

Source slugs in uwuwu: `Projects/foilcap/ADR/ADR-028: Таблицы чата рендерит
приложение над живым h-скроллом; тонкие скроллбары глобально`,
`Projects/foilcap/ADR/ADR-030: Дизайн-система ai-toolbox-ui — vendored-subset
от craft-launcher`, and the foilcap UI-fix runbook.
