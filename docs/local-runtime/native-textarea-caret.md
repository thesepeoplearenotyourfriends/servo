# Native editable-control mouse positions

The textarea correction continues PR #55 (`fix/textarea-click-scroll-offset-20261023`) and integrates main through `c58f482d4d1a3ff33071db2740493ba8ef4ab887`, which already contains the separate document-highlight cleanup fix. It does not reimplement that fix or modify its flag lifecycle.

## Coordinate findings

`TextInput::handle_mouse_event` resolves native single clicks and drags through `edit_point_for_mouse_event` → `Window::text_index_query_on_node_for_event` → `find_character_offset_in_fragment_descendants`. The event supplies CSS viewport pixels; the window converts them to app units and ensures the stacking-context tree is current before querying.

`StackingContextTree::offset_in_fragment` applies the root-to-spatial-node inverse transform and removes the root fragment's cumulative content origin. That handles ancestor scrolling and the control's CSS transform. The descendant walk then subtracts each child's content origin exactly once. Text scoring and glyph queries therefore receive points relative to the text fragment's own content origin.

PR #55's scroll correction is retained: a box's own spatial node precedes the scroll frame it generates for its children (`BoxFragment::build_stacking_context_tree_for_children`). Descending into those children must add that generated frame's offset. `ScrollableNodeInfo` stores positive content offsets in CSS pixels; node-to-parent transforms subtract the offset and the inverse adds it. The candidate converts this vector to app units before adding it. It uses only the generated frame at each descent, so the ancestor transform is not applied twice. A zero offset adds nothing.

The candidate alone is insufficient. `TextFragment::point_is_within_vertical_boundaries` and `distance_to_point_for_glyph_offset` compared the already-local point with `base.rect()`'s parent-relative origin. This effectively applied a line/column displacement twice when ranking fragments, even at scrollTop zero. `character_offset` also used the parent-relative bottom edge for its below-fragment cutoff. These checks now use zero-to-size bounds. Fragment geometry, line construction, shaping, and text-index conversion remain unchanged.

Double- and triple-click default handling previously expanded word/line boundaries around the existing `edit_point` without resolving the current mouse point. Both now resolve the mouse point through the same query as single clicks and drags, then use the existing Rope boundary operations. Logical line-selection semantics and drag extension behavior are preserved.

## Scroll history and viewport feedback

Programmatic scroll goes through `Window::perform_a_scroll` and layout's `set_scroll_offset_from_script`, updating the layout scroll tree and sending the renderer its new offset. Wheel scroll updates the renderer's tree and feeds offsets back through `ScriptThread::handle_set_scroll_states` → `set_scroll_offsets_from_renderer`. Both tree-update paths invalidate cached transforms. Tree rebuilds preserve existing scroll offsets. No separate programmatic-versus-wheel synchronization defect was established by this investigation; the new engine regression compares both paths at the same offset.

The inspected native textarea mouse path resolves the index, updates `TextInput`, updates `ScriptSelection` in `HTMLTextAreaElement::maybe_update_shared_selection`, and requests a display list. Selection painting does not scroll to a caret, and native mouse focusing calls the focusing steps without the keyboard-focus scroll helper. There is no old-caret scroll request to suppress in this path. A follower that reveals the incorrectly resolved index could reproduce the reported jump, but the original follower probe/application is not present here and that connection is **not confirmed**. No generic scroll suppression or new automatic-caret-scrolling system was added.

The concrete configuration distinction established locally is between parent-relative and fragment-local origins: a fragment at `(0, 0)` can mask the scoring error, while later lines or displaced text expose it. Scrolled descendants additionally require their own generated scroll-frame offset. The historical A/B/C probe values alone do not identify fragment origins or establish why B differed after wheel scrolling. Those exact empirical outcomes, caret-induced scrolling, and the original follower/composer behavior still require runtime reproduction.

## Regression coverage and local receipt (2026-10-23)

- Two layout unit tests in `fragment_tree/fragment.rs` check translation-invariant local scoring and the below-fragment cutoff. An isolated build compiles the exact geometry methods and test bodies with real `app_units`/`euclid`, adapting only fragment construction and empty glyph storage: both fail on main and pass with the correction. This does not type-check the full layout crate or its real style/font construction.
- Three isolated gesture checks compile the exact `handle_mousedown` method with production Rope and Unicode code; DOM events and the layout hit result are injected and heap-accounting derives are omitted. Word/line cases fail before the change and pass afterward; single-click behavior passes in both. These checks do not dispatch actual native events.
- `textarea-caret-hit-testing.html` adds eleven engine WPT cases using loaded Ahem metrics, known ASCII offsets, trusted pointer/wheel input, and DOM selection assertions before/after rendering opportunities. Cases cover an unscrolled middle line; programmatic scrolling with an offscreen old caret; padding/borders; a nested scroller and scaled control; wheel/programmatic equivalence; horizontal plus vertical scrolling; trailing empty space; wrapped-line ends; double/triple click with a selection changed between clicks; and single-line input plus native typing. Scroll assertions allow one CSS pixel of rounding, not a jump to an old row. No test changes mouse coordinates in an event handler or cancels native handling.
- New WPT JavaScript syntax and WPT lint pass. Existing input/textarea mouse-selection WPT files also pass lint. Modified Rust modules parse, new Rust test code is formatted, and diff whitespace checks pass. Existing unrelated formatter differences are preserved.

These are source/isolated-test receipts. The new full-engine unit tests and WPT cases have **not run** in this session. The selected cloud workspace has no warm Servo target or runtime executable. GitHub workflow dispatch is unavailable in the connector, and a shell GitHub API check with network permission returns `Forbidden`; no warm build/test run was dispatched. No cold script/layout/workspace build was attempted.

Use the existing **Warm Debian 12 Command** workflow on the updated PR #55 branch with one integrated pass:

```sh
set -Eeuo pipefail
cargo test --locked --release -p servo-layout --lib caret_hit_test
./mach build --release
./mach test-wpt --release /_mozilla/input-events/textarea-caret-hit-testing.html
./mach test-wpt --release /_mozilla/input-events/mouse-text-selection-input-text.html
./mach test-wpt --release /_mozilla/input-events/mouse-text-selection-textarea.html
```

After engine tests pass, repeat the physical native/follower textarea and composer cases, including equivalent wheel/programmatic/caret-induced scroll positions, multi-click targeting, and scrolling the old caret out of view. Record actual clicked coordinates, resolved indices, and before/after scroll offsets using sanitized fixture identifiers. Physical presentation, the original viewport jump, and real copy/paste remain unverified here. No new runtime diagnostics or user-text logging are introduced.
