# Immediate mode: state, IDs and repaint

Source: `egui` 0.37 API, the egui 0.37 experience article in uwuwu, and the
foilcap bridge incidents recorded there.

## Frame lifecycle

1. `eframe::App::update(&mut self, ctx, frame)` runs once per painted frame.
2. Widgets are functions of current state. The UI is rebuilt each frame.
3. egui produces shapes. The backend renders them.
4. The app sleeps when no input or repaint request is pending.

Keep domain state in your app struct. Do not store it in widgets. A widget
returns a `Response`; read `clicked()`, `hovered()` or `dragged()` in the same
frame.

## Identity

- Use `Id::unique(("answer", index))` for repeated viewers. One shared ID makes
  two markdown viewers collide in `ViewerCache` and split-point state.
- `Id::new` is deprecated in 0.37. Its replacement hashes with fixed seeds and
  stays stable across frames.
- Use `ui.push_id` when a loop creates widgets with the same label.
- `Context::data_mut` stores temporary (`insert_temp`) and persisted
  (`insert_persisted`) state. Use `Memory::get_temp` and `insert_temp` when a
  layout needs a value measured in the previous frame.

## Input

`ctx.input(|i| ...)` is a snapshot of the current frame. `i.key_pressed(Key::X)`
is true only on the frame the key went down. It is not a held-key test. There is
no event bubbling: several call sites can read the same key in one frame.

## The click that arrives with new data

egui hit-tests a click against rectangles from the **previous** frame. A new
widget cannot receive a click on the frame it first appears. If a bridge reply
wakes the UI on the same frame as the click, the click is lost. The hover
highlight is not proof that the click was delivered.

Fix the class, not the instance:

1. Send a `Context::request_repaint` when any bridge data arrives. The call is
   thread-safe and egui debounces it.
2. Use a small `EguiRepaintSignal` (`Arc<Mutex<Option<Context>>>` with `set_context`
   and `notify`). `notify` is a no-op before the first frame.
3. Pin the race in a kittest test: sleep several frames with an empty list, then
   send a pointer move, press, release and the data in one step.

`has_requested_repaint` is a bad marker: a fresh Context already has an
outstanding repaint. Count calls with a public `set_repaint_observer` instead.

## Panel API in a harness

The bottom panel can render with zero content under eframe + egui_kittest. Put
the composer in an explicit block inside `CentralPanel` with a `max_height`
`ScrollArea` instead of relying on `BottomPanel`.

## Checklist

- [ ] State lives in the app struct, not in widgets.
- [ ] Repeated IDs use `Id::unique` with a per-instance key.
- [ ] Async data triggers `request_repaint`.
- [ ] A first-click test uses raw pointer events plus a queued reply.
- [ ] The harness uses an explicit composer block.

Source slugs in uwuwu: `Experience/rust/egui 0.37 Stack Gotchas - kittest
Snapshots, commonmark source_id, Panel API, wasm-bindgen Pin`.
