# Native document text selection

Ordinary document text uses the engine's DOM `Selection` and `Range`. Platform mouse events establish and extend that range, layout maps its boundaries through whitespace/case transformations, and painting draws the selected glyph intervals. The HTML application needs no selection script or editable overlay.

## Adapted upstream work

- [#46698](https://github.com/servo/servo/pull/46698): document selection flags, flat-tree traversal, range invalidation, and default highlight style.
- [#47048](https://github.com/servo/servo/pull/47048): finding a nearby text fragment and converting a glyph position to a DOM endpoint. This fork keeps its existing layout query interface rather than importing the later event/hit-test interface refactor.
- [#47358](https://github.com/servo/servo/pull/47358): collapse at primary mousedown and extend while the primary button remains pressed. The fork keeps form controls on their existing handlers; canceled pointerdown/mousedown and editable/control targets do not start document gestures.
- [#47579](https://github.com/servo/servo/pull/47579): inspected the later selection representation and invalidation corrections; retained the explicit distinction between a document highlight and an editing caret, corrected first-letter range intersection, and bounded reverse-mapped endpoints to their originating node. The broad input-type refactor and paint-only selection optimization are deferred; document range changes currently invalidate affected text layout.
- [#48101](https://github.com/servo/servo/pull/48101): document copy as a default editing action through the existing trusted clipboard event and `EmbedderMsg::SetClipboardText`. Context menus, Select All, and the later editing subsystem refactor are not imported.
- Necessary prior art: [#46864](https://github.com/servo/servo/pull/46864) for the whitespace/case iterator and `OffsetMap`; [#46963](https://github.com/servo/servo/pull/46963) for keeping text fragments associated with one DOM text node; [#47022](https://github.com/servo/servo/pull/47022) and [#47025](https://github.com/servo/servo/pull/47025) for forward/reverse offset mapping; [#47937](https://github.com/servo/servo/pull/47937) for line-box highlight geometry.

The pinned Rust toolchain, Stylo revision, Python bridge, file-opening code, shell presentation modes, and resource/transport policies are preserved. The lockfile gains `arrayvec` for the adapted layout iterator and the already-declared `severin-python` dependency on `winit`, which was missing from the baseline lockfile.

## Paths

- `components/script/dom/selection.rs`, `dom/range`, `dom/node`, and DOM/layout accessors: live range, direction, selection flags, mutation invalidation, UTF-16 boundaries, and layout-safe traversal.
- `components/script/dom/document/document_event_handler.rs` and `dom/window.rs`: native gesture/default-copy dispatch and the text-position layout query.
- `components/layout/flow/inline/{construct,text_run,line,text_transform,offset_map}.rs`: whitespace/case mapping, original-node identity, fragment offsets, and boundary tests.
- `components/layout/{dom_traversal,flow/construct,construct_modern,table/construct}.rs`: propagate document text ranges through existing layout construction.
- `components/layout/display_list/{mod,paint_traversal}.rs`, `query.rs`, and `stylesheets/servo.css`: highlight geometry, hit positions, and a default selection background.

## Local fixture and GUI checks

Load `tests/html/local-runtime/document-selection/index.html` through the existing package-scoped opening path. The fixture contains no JavaScript and needs no network resources. Keep using Severin's existing Python-managed opening path and `--no-egui` behavior; no new opening command is introduced here.

1. Drag from the top line, including its first few pixels; repeat across nested spans and wrapped lines, forward and backward. Confirm highlighting updates while pressed and survives release.
2. Inspect `window.getSelection().toString()`, anchor, and focus through the runtime's existing diagnostics. Include the non-BMP fox character and adjacent text separated by a comment.
3. Use Ctrl+C on Linux/Windows or Command+C on macOS. Focus the input and paste, then verify input-native selection/copy still works.
4. Click links and buttons normally; drag link text and confirm the link is not activated. Verify ordinary input focus and editing.
5. Repeat after scrolling/resizing and at the intended display scale. Check `user-select: none` only when supported by the pinned runtime's CSS preferences.

The normal desktop shortcut already dispatches `EditingActionEvent::Copy`; toolbar geometry already becomes zero in the existing bare mode. These shell paths are not modified.

## Automated validation

Focused checks that do not build the browser:

```sh
cargo test --locked -p servo-base text::
cargo test --locked -p servo-url --test local_runtime_raw_url
```

`offset_map.rs` is independent of layout/style and contains five tests. It can be compiled directly with `rustc --test` against the artifacts from `cargo build --locked -p servo-base`, supplying the `servo_base`, `malloc_size_of` (the `servo_malloc_size_of` artifact), and `malloc_size_of_derive` externs plus the target's debug dependency directory. This runs the production mapping module without compiling `servo-script`.

On the established warm GitHub build infrastructure, run:

```sh
cargo test --locked -p servo-layout --lib offset_map
./mach build --debug
./mach test-wpt --debug /_mozilla/input-events/mouse-document-selection.html
./mach test-wpt --debug /_mozilla/input-events/document-selection-paint.html
./mach test-wpt --debug /_mozilla/input-events/mouse-text-selection-input-text.html
./mach test-wpt --debug /_mozilla/input-events/mouse-text-selection-textarea.html
```

Use `.github/workflows/warm_command.yml` to execute checks against the requested branch/commit in the restored warm environment. Do not launch a cold full-workspace build in the agent environment.

## Validation receipt and limits

Current cloud checks: four Unicode/base tests passed, five production offset-map tests passed, and four existing package-isolation URL tests passed. Cargo workspace metadata resolves with `--locked`. The new WPT files pass the repository's WPT lint. Rust parsing/formatting and focused source-tidiness checks are recorded in the progress log.

These checks do **not** establish that `servo-script`/layout/servoshell type-check together or that physical mouse/highlight/system clipboard behavior works. A warm engine build, the new WPT runs/reftest, and physical GUI/clipboard checks remain required before a readiness claim. GitHub API access was denied by the environment proxy during implementation, preventing workflow dispatch and PR creation until that destination is enabled.

The pinned Stylo gates `user-select` behind `layout.unimplemented`; the selection code honors computed restrictions when available, without enabling unrelated unfinished CSS. Vertical writing, complex bidi/ligature caret affinity, generated content/first-letter hit endpoints, cross-shadow-root selection, and drag autoscroll retain browser-engine limitations. Copy/stringification uses the existing Range stringifier, so whitespace follows DOM text and style-dependent rendered-text serialization is not newly implemented. This change does not implement selected-text foreground colors or a general editing/caret system.
