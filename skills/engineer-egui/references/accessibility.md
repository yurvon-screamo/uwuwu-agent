# Accessibility in an egui application

Paraphrased from `storytold/craft-contrib` `standards/accessibility.md` at
revision `646f558b8a9c0319faea6c938454634de5054864`. That repository has no
license file at its root, so the rules are restated here, not copied. Follow
the link for the full text.

Assistive technology reaches a native egui app through AccessKit: UI Automation
on Windows, NSAccessibility on macOS and AT-SPI on Linux. The target is WCAG
2.2 AA as applied to software by WCAG2ICT.

## Rules for every UI change

1. **Keyboard.** Every action works from the keyboard, in visual order, with no
   trap. Esc closes a popup and returns focus. Shortcuts can be turned off or
   remapped.
2. **Real focus.** Keyboard navigation moves egui's focus with `request_focus`.
   A painted highlight alone is not focus. The focus stays visible and is not
   covered by other UI.
3. **Name, role, state.** Each control exposes its role, a name that starts with
   its visible label, and its state: checked, pressed, selected, expanded or
   disabled. Put the shortcut on the node in platform form.
4. **Spoken feedback.** Announce changes away from focus through a live region:
   a shortcut toggle, "Saved", a result count, an error. Errors say what is
   wrong in text. Messages that disappear can be read again.
5. **Drawn surfaces.** Publish painter content as AccessKit nodes: text with
   caret and selection, cells with positions, objects with alt text and the
   caret's screen position for magnifiers. Mark an editable surface editable.
6. **Seeing.** Text contrast is at least 4.5:1. Controls and focus indicators
   are at least 3:1. Check both themes. Colour is not the only signal. Follow
   system high contrast, text size and reduced motion settings.
7. **Pointer.** Anything done by dragging also works without dragging, unless
   the drag is the task. Targets are at least 24 by 24 points or have that much
   space around them.

## Design before code

Write down, in the issue or PR:

- the standard pattern (menu bar, tab list, toolbar, tree, grid, list, dialog,
  slider, canvas) and its keyboard model;
- how a reference app exposes the same pattern, observed with a platform
  inspector under the clean-room rule;
- how keyboard, screen-reader and magnifier users complete the task.

Record observed behaviour in the project's parity docs.

## Checks

- Drive keyboard paths in egui_kittest and assert the accessibility tree:
  roles, names, states, focus after each step and announcements.
- Keep the built-in accessible-name check on. A test that calls
  `with_accessibility_check(false)` must say why.
- Check contrast of new colours in both themes. Compute it; do not eyeball it.
- Read the accessibility tree together with every `ui.screenshot`.
- CI must check what the platform reports, not only the tree. A correct tree
  can still reach a screen reader incorrectly.

A UI PR carries an Accessibility section. Tree tests alone do not support a
claim that the app works with screen readers.

## Known failures to look for

- focus left behind a menu or dialog;
- an editable surface reported as read-only;
- a dialog that does not take or return focus;
- painted rows or handles without nodes, or with the same name;
- no visible focus;
- a state change nobody hears;
- platform text in a name, such as "Paste (⌘V)" on Windows;
- an action available only by dragging.
