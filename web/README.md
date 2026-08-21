# UBC125 Web Console

Browser UI for the UBC125XLT, a faithful translation of the terminal console
(`src/cmd/console.rs`) to the browser. Talks to the `ubc125 serve` gRPC
server over **grpc-web** on the same port — no dev server, no build step.

## Layout

```
web/
  dist/            # the app — committed, embedded into the binary at build time
    index.html     # layout skeleton + import map (pinned CDN ESM URLs)
    app.js         # state object, tab switching, global keymap, action wiring
    theme.css      # the whole look: palette, boxes, tab bar, table, modals
    lib/client.js  # connect-web (grpc-web) clients — the only transport code
    lib/freq.js    # pure frequency helpers (mirrors src/types.rs)
    lib/audio.js   # AudioStream: gRPC Listen → MediaSource playback (see Audio)
    lib/backoff.js # bounded exponential backoff (audio reconnect)
    components/    # box, tabbar, helpbar, modal (titled ratatui-style boxes)
    views/         # monitor.js, bank.js — one module per view
    proto/         # generated protobuf-es JS (committed)
    tests/         # node --test unit tests for the pure logic (freq, bank, backoff, audio)
  node_modules/    # dev-only (protoc-gen-es for codegen); not used at runtime
  package.json
```

There is no `src/` — development happens directly in `dist/`. The directory
is committed for the same reason the generated Rust proto code is: the
release build (and the Nix flake) must not need a node toolchain.

## Dependencies

No runtime build. `index.html` loads two ESM packages from jsdelivr via an
**import map** (plan decision D8: import maps chosen over an esbuild
bundle — the app is small, and this keeps the Nix build node-free). Pins
(all verified reachable as ESM):

- `@bufbuild/protobuf@2.14.0`
- `@connectrpc/connect@2.1.2` + `@connectrpc/connect-web@2.1.2`
  (the grpc-web protocol; there is no `@grpc/web` on npm)

Consequence: the UI needs internet access on first page load (CDN). If
offline/intranet use ever becomes a requirement, vendor the two packages
into `dist/vendor/` with a single `esbuild --bundle` and point the import
map at the local files.

## Running

```sh
cargo run -- serve                 # or a prebuilt binary
# open http://localhost:50051/
```

The app talks to `location.origin` by default. For cross-origin dev:
`http://localhost:50051/?server=http://other-host:port` (server CORS is
permissive).

`dist/` is embedded at compile time in **both** debug and release builds
(`rust-embed` is used without its `debug_mode` feature), so web changes
need a `cargo build` and a stack restart — there is no hot reload in any
build.

## Tests

```sh
cd web && node --test dist/tests/*.test.js
```

31 tests cover the pure logic (`freq.js`: frequency parsing/normalization
and display formatting; bank labels/ranges/cursor math; the backoff
schedule; audio chunk bookkeeping). Browser-level verification is scripted
in `tests/web_e2e.md` (run with the browser-tools skill against the fake
scanner) — not part of `node --test`.

## Audio

The Monitor view's Play/Stop (`p`/`x`) stream the scanner's audio through
`AudioStream` (`lib/audio.js`):

- `AudioService/Listen` gRPC stream → WebM/Opus `AudioChunk`s. The first
  chunk (`initSegment: true`) is the WebM header; each generation (fresh
  Play, or reconnect) starts a new `MediaSource` + `SourceBuffer`.
- The `MediaSource` is attached with `audio.src =
  URL.createObjectURL(mediaSource)` — Chromium/Edge rejects
  `srcObject = <main-thread MediaSource>` (only `MediaStream` / worker
  `MediaSourceHandle` are accepted there). The object URL is revoked on
discard/teardown.
- Bounded buffering: the append loop drains while `updateend` is pending;
  trim keeps ~3 s behind the playhead and **caps the tail at ~10 s ahead**
  (a faster-than-real-time source without the cap wedges the tab). Trim and
  append must not interleave on one pass (`InvalidStateError`) —
  `_trimIfNeeded()` reports whether it trimmed and the loop `continue`s.
- On stream failure the state is `reconnecting` (bounded exponential
  backoff, `lib/backoff.js`), never `unavailable`, until the stream ends
  with a terminal error (e.g. capture stopped) — then Stop is re-enabled
  and the state is `off` after Stop/StopCapture.
- Stop is explicit: `AudioStream.stop()` fires `StopCapture` (fire-and-
  forget) because aborting the fetch does not close Chrome's keep-alive
  socket — server-side the capture would otherwise keep running (and keep
  holding the ALSA device).

## Proto codegen

`dist/proto/` is generated from `lib/grpc/proto/ubc125/v1/services.proto`
with protobuf-es. After changing the `.proto` file:

```sh
cd web
protoc --plugin=protoc-gen-es=$PWD/node_modules/.bin/protoc-gen-es \
  --es_opt=target=js --es_out=dist/proto \
  -I ../lib/grpc/proto \
  ../lib/grpc/proto/ubc125/v1/services.proto
UBC125_REGEN=1 cargo build -p ubc125-grpc   # Rust side
```

Commit the updated `dist/proto/` (and `lib/grpc/rust-gen/src/proto/`).
`protoc` and `nodejs` are available via `nix-shell -p`.

## Conventions

- Vanilla ES modules, no framework, no TypeScript. JSDoc on non-obvious
  functions. One `state` object in `app.js`; views are pure render
  functions; keys and pointer events funnel into the same action
  functions.
- protobuf-es v2 plain objects are **camelCase** (`signalDetected`,
  `channelName`); the server sends display-format frequencies — normalize
  with `fromUserInput()` on receipt, send raw 8-digit strings.
- `stripPrefix()` removes the scanner's `CMD,` echo from raw responses
  (`MDL,UBC125XLT` → `UBC125XLT`).
- The stack script runs the prebuilt **debug** binary, but rust-embed (no
  `debug_mode` feature) embeds `dist/` at compile time in debug **and**
  release builds — so any web change needs `cargo build` + a stack
  restart, same as a Rust change.
- Focus events are suppressed while the browser window is unfocused, so
  modal field highlighting is driven explicitly (`setActive`), not by
  focus/blur alone.
