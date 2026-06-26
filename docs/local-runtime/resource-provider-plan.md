# Local Runtime Resource Provider Plan

This document sketches the first implementation path for turning Servo into a host-mediated local document/application runtime. The goal is not to delete networking first; it is to make all resource acquisition explicit and host-owned.

## Core Model

Servo should request resources through a contextual request object. The host resolves, authorizes, and returns bytes or a deterministic denial.

```rust
pub struct ResourceRequest {
    pub requested_url: Url,
    pub base_url: Option<Url>,
    pub initiator_url: Option<Url>,
    pub package_id: PackageId,
    pub origin: RuntimeOrigin,
    pub destination: ResourceDestination,
    pub mode: LoadMode,
    pub credentials: CredentialMode,
    pub cache_mode: CacheMode,
}

pub struct ResourceResponse {
    pub final_url: Url,
    pub mime_type: Mime,
    pub bytes: ResourceBody,
    pub cache_policy: CachePolicy,
    pub integrity: Option<IntegrityMetadata>,
    pub source_metadata: SourceMetadata,
}
```

The important inversion is: Servo may ask for resources, but only the host decides what is reachable.

## Initial Scheme Policy

Allowed in the first milestone:

- `asset://{package_id}/...` for package-relative resources rooted inside the active package.
- `bundle://runtime/...` for immutable runtime-owned resources.

Denied in the first milestone:

- `http://`
- `https://`
- `ws://`
- `wss://`
- `ftp://`
- `file://`
- `store://` as a fetchable resource
- `asset://` URLs for another package
- path traversal outside the package root

Deferred until real content requires them:

- `data:`
- `blob:`

## Request Flow

Every resource request should follow the same host-controlled path:

1. Receive a `ResourceRequest` from Servo.
2. Resolve `requested_url` against `base_url` when necessary.
3. Normalize and canonicalize the resolved URL.
4. Classify scheme and destination.
5. Check package, origin, and capability policy.
6. Load from the package or bundle backend.
7. Return `ResourceResponse` or a deterministic error.

Policy denials should be first-class outcomes, not lower-level network failures.

Initial instrumentation is deliberately late: `components/net/resource_thread.rs` logs built Fetch requests before they enter the legacy fetch/http/protocol path. The first v0 package implementation also lives at this seam. With `SERVORENA_PACKAGE_ID` and `SERVORENA_PACKAGE_ROOT` set, it authorizes `asset://{active_package_id}/...`, rejects raw and single-percent-decoded `..` traversal before mapping the path to a canonical file under the package root, returns bytes with simple extension-derived MIME, and denies package-mode remote HTTP(S), WebSocket, file, store, cross-package asset, missing file, I/O, and traversal/root-escape failures before legacy dispatch. This is intentionally not a package manager and not the final `ResourceProvider` boundary. In package mode, fallthrough beyond this wall is now treated as a bug/classification gap: known deny schemes return deterministic denials, and not-yet-routed schemes such as `bundle:`, `data:`, and `blob:` log `unsupported-unrouted` and complete with a deterministic network-error response before legacy fetch/protocol handling. Later work should move request construction earlier so original requested text, base URL, initiator URL, and destination-specific MIME errors are preserved more precisely.

A first earlier guard now exists in `ServoUrl` parsing/joining: raw requested text is inspected for path-boundary obfuscation before normalization. Ordinary percent encoding is still accepted, while encoded `.`, slash, backslash, NUL, and double-encoded forms of those bytes are logged and denied when package mode is enabled. This is a stopgap visibility/policy seam; the final provider request should carry both raw requested text and normalized final URL so denial reasons do not depend on URL parser error variants.

## Error Categories

The provider should distinguish at least these outcomes:

- `DeniedByPolicy` for disallowed schemes, capabilities, or cross-package access.
- `UnsupportedScheme` for schemes the runtime does not implement.
- `InvalidPath` for traversal or canonicalization failures.
- `NotFound` for missing package or bundle resources.
- `InvalidMime` for a resource that does not match the destination.
- `DecodeError` for bytes that cannot be consumed by the destination decoder.
- `IoError` for backend failures.

Good error categories are part of the developer experience. A missing local image should not look like a denied remote URL.

## First Milestone Package

The first acceptance package should be deliberately small:

```text
app/
  index.html
  styles.css
  main.js
  assets/logo.png
  fonts/app.woff2
```

The runtime should load:

- `asset://com.example.app/index.html`
- `./styles.css` from the document
- `./assets/logo.png` from HTML
- `./assets/logo.png` from CSS `url(...)`
- `./fonts/app.woff2` from `@font-face`
- `./main.js` as a classic script or module script

It should deterministically reject remote URLs, `file://`, traversal attempts, and `store://` fetches.

## CSS Subresource Context

CSS subresources must preserve two related but distinct URLs: the active document initiator and the stylesheet base that resolved the nested CSS reference. `@import` currently enters Servo through `components/script/stylesheet_loader.rs`, after Stylo resolves the imported URL against the parent stylesheet `UrlExtraData`; it is then fetched as `Destination::Style` and reaches the existing package wall. `@font-face src: url(...)` enters through `components/fonts/font_context.rs`; stylesheet-initiated font fetches now use the parsed stylesheet URL as the fetch referrer/base context while retaining the document URL as initiator context in local-runtime logging.

The final provider request shape should make this explicit instead of overloading referrer:

- `destination`: `Style`, `Font`, or `Image`
- `requested_url`: original CSS token text when available
- `base_url`: stylesheet URL for `@import`, `@font-face`, and CSS image `url(...)`
- `initiator_url`: active document URL
- `final_url`: resolved URL after stylesheet-relative resolution

CSS image `url(...)` remains an open mapping item.

## URL Resolution Provenance

Source-level provenance logging now exists before the final resource-thread policy wall for package-relevant document, external script, and module URL resolution. The important distinction is:

- `author_text` means the string is still visible at an owning document/module seam and is believed to be raw author input, such as a script `src` attribute or module specifier string.
- `resolver_input` means the shared URL parser/join layer sees a string, but the caller may already have decoded, rewritten, normalized, or otherwise transformed it.

Future `ResourceRequest` work should carry both the raw author spelling, when available, and the resolved `ServoUrl`. Without that request-shape change, final resource-policy logs can correlate by adjacent source-seam logs but cannot always prove which raw spelling produced a later normalized URL.

## Python Embedding Boundary

An earlier CPython native extension crate lives at `ports/severin-python/` as experimental/manual in-process embedding work. Its import identity is now distinct from the visible launcher (`severin_embedded` rather than `severin`) so it cannot shadow the normal pure-Python launcher/controller package.

The current `load_path` implementation is intentionally narrow and maps one entry file onto the existing first-milestone `asset://com.example.app/...` package wall. The final provider should replace that process-global handoff with an explicit package/context object before supporting multiple simultaneous Python `App` instances.

The normal Python wheel path now packages the pure-Python `severin` launcher/controller. It does not contain the in-process native extension, does not embed Servo in Python, and does not introduce a helper listener, port, socket, daemon, localhost service, or network bridge.

The Python bridge is a transport queue, not an application protocol. Page JavaScript submits arbitrary serialized JSON through `globalThis.severin.send(value)` and receives a Promise that resolves with the parsed JSON reply. Python reads `(opaque_receipt, json_text)` from the inbound queue and writes arbitrary valid JSON back against that private receipt. Receipts are native-only, single-use, and bound to the originating top-level document identity so navigation or teardown prevents delivery into a later document. The native layer must not define action names, capability names, permission rules, success/error conventions, request schemas, reply schemas, or a registry of host functions. Its only owned failures are transport/lifetime failures such as `App.close()`, document teardown, or an expired reply target. See `docs/local-runtime/python-embedding.md` for the current API and bridge transport model.

## Severin Python bridge transport envelope

The Severin JavaScript ↔ Python bridge is a bounded, one-shot, per-document request/reply mailbox. It is payload-neutral: Rust never assigns semantic meaning to JSON contents, never branches on application fields such as `ok`, `error`, `op`, MIME labels, or app-defined payload kinds, and never treats base64-encoded data, including images, as a special transport. Every valid JSON root value is ordinary bridge data subject only to serialized UTF-8 byte counts and mailbox state.

A normal `severin.App(width=..., height=...)` receives a complete finite default resource envelope without configuration ceremony. Python may optionally pass `bridge_limits=` as a keyword-only partial mapping override; omitted keys inherit defaults, unknown keys fail at construction, invalid non-positive or oversized values fail at construction, and pages cannot inspect or alter the resulting App-lifetime envelope. If `max_frame_bytes` is raised and dependent byte ceilings are omitted, the effective `byte_burst` and `max_queued_bytes` normalize upward enough for one such frame; explicitly contradictory ceilings fail clearly. These limits are process-health controls, not an application protocol, content policy, RPC vocabulary, or data-authorization system.

Default bridge limits are: `max_frame_bytes` 1 MiB, `max_live_receipts` 128, `max_queued_frames` 128, `max_queued_bytes` 8 MiB, `messages_per_second` 256, `message_burst` 128, `bytes_per_second` 8 MiB, `byte_burst` 2 MiB, and `max_deliveries_per_pump` 32.

Admission from page to Python checks only JSON serializability, exact serialized UTF-8 byte size, receipt capacity, page-side and native unread-frame capacity, queued-byte capacity, configured message/byte rate and burst envelopes, and active document generation. The native-created shim mirrors the physical frame and staging limits before retaining outbound frames so bridge staging cannot grow without bound; Rust repeats the checks as the authoritative boundary and folds private shim rejection deltas into the same App-lifetime debug ledger on the next successful drain. Oversize admissions reject the JavaScript Promise with `BridgeTooLargeError`; receipt, queue, or rate pressure rejects with `BridgeBackpressureError`. Rejected admissions create no receipt and enter no Python-visible queue.

Receipts are opaque, one-shot, App-lifetime nonce tokens scoped to the active document generation. `read()` removes one unread frame but does not consume the reply right. Python replies may arrive in any order, but a live receipt can settle only once; unknown, settled, stale, or invalidated receipts fail on `write()`. `write()` validates reply JSON and applies `max_frame_bytes` before consuming the receipt, so invalid JSON or an oversized reply leaves the receipt live for a later valid reply. Application error-shaped JSON fulfills the originating Promise as ordinary data; native transport errors are the only bridge-created Promise rejections.

Navigation and close are terminal bridge boundaries. `load_path()` terminally invalidates the old generation, discards unread old frames, makes old receipts unusable, prevents old reply targets from reaching the replacement document, advances generation once, and leaves the replacement document identity unknown until its first successful drain claims it. `close()` is idempotent, and explicit close or native window close clear pending evaluations, unread frames, and live receipts before later App operations fail through the documented closed-App behavior. Explicit `BridgeNavigatedError` and `BridgeClosedError` rejection names are reserved for a future case where an old realm is still live enough to observe native cancellation. App use is single-owner and explicit-pump: public App methods reject calls from non-owner Python threads, `read()` is nonblocking, and each `pump()` delivery pass is bounded by `max_deliveries_per_pump`.

`app.bridge_debug_state()` is a compact diagnostic/probe surface only. It returns a Python mapping with the current document generation, effective limits, current live receipt count, current queued frame/byte counts, the delivery count for the most recent pump turn, App-lifetime rejection counters including private shim admission rejections folded at drain time, stale reply rejection count, and App-lifetime peaks for live receipts and queued bytes. It is not page-visible telemetry and does not define an application protocol.

## Visible Severin runtime pivot: headed child + private inherited FDs

The visible desktop runtime is now the single normal headed Severin executable, built from the existing `ports/servoshell` headed winit/Servo lifecycle. It supports two modes:

```text
severin --severin-package-root /abs/app --severin-entry index.html
severin --severin-package-root /abs/app --severin-entry index.html \
  --bridge-request-fd=<child-write-fd> --bridge-reply-fd=<child-read-fd>
```

The bridge FDs are optional inherited anonymous pipe handles. They are not a listener, address, port, WebSocket, HTTP endpoint, TCP/UDP socket, named Unix socket, or public IPC service. Both bridge FD options must be supplied together or omitted together.

Python is now a launcher/controller for this same executable. It creates two one-way anonymous pipes, passes only the child ends with `pass_fds` and `close_fds=True`, reads child request frames, invokes the application callback with `(receipt, json_text)`, and writes replies through a single serialized writer thread. Python does not own Servo, winit, X11, input, timers, canvas, redraw, paint, or present.

The private frame format in both directions is:

```text
[u32 big-endian JSON byte length][u64 big-endian receipt][UTF-8 JSON bytes]
```

The receipt remains transport-private and outside application JSON. Rust keeps only transport/lifetime limits: frame size, live receipts, queue/byte limits, rate limits, stale receipts, navigation/close invalidation, and bounded delivery work. It does not define action names, capability names, schemas, success/error envelopes, or host-function registries.

`ports/severin-python` remains experimental/manual/headless/in-process embedding work under a distinct native-extension import identity. It is no longer the visible desktop rendering foundation and should not grow a replacement headed GUI lifecycle.
