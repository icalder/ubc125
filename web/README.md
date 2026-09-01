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
  smoke.mjs        # gRPC-Web smoke test against `serve` (node, no browser)
  run_smoke.sh     # fake-scanner stack + smoke.mjs, then teardown
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

### Working on the pages locally (no scanner required)

Do **not** open `dist/index.html` with a double-click: browsers refuse to
load ES modules (and the import map) over `file://`, so `app.js` never runs
and you get a blank page with a "module load" error. Serve the folder over
HTTP instead. `dist/` is a flat tree of static files, so any static server
works — `python3` is sufficient:

```sh
cd web/dist
python3 -m http.server 8137
# open http://localhost:8137/
```

(Or a Node dev server, `npx --yes serve -l 8137 .`, if you prefer.) The UI
renders fine against a **down** backend — it just shows "OFFLINE — waiting
for scanner..." in the help bar, since there is no gRPC server. That is fine
for editing pages. To exercise real RPCs without hardware, run the fake
scanner stack (`bash tests/ubc125_stack.sh`) and open
`http://localhost:8137/?server=http://127.0.0.1:50051`.)

## Tests

```sh
cd web && node --test dist/tests/*.test.js
```

31 tests cover the pure logic (`freq.js`: frequency parsing/normalization
and display formatting; bank labels/ranges/cursor math; the backoff
schedule; audio chunk bookkeeping). Browser-level verification is scripted
in `tests/web_e2e.md` (run with the browser-tools skill against the fake
scanner) — not part of `node --test`.

Between the two sits a client-side protocol smoke test: it drives the real
grpc-web transport (the same `@connectrpc` packages the browser uses) from
node against the fake scanner — no browser, no page.

```sh
bash web/run_smoke.sh          # stack up, 4 checks, stack down (~3 s)
bash web/run_smoke.sh --keep   # leave the stack up for further poking
node web/smoke.mjs http://host:50051   # checks only, against a running server
```

`run_smoke.sh` reuses `tests/ubc125_stack.sh` (socat pty pair →
`tests/fake_scanner.py` → `serve` on `127.0.0.1:50051`), builds the debug
binary if missing, and kills the stack afterwards unless `--keep`. Logs go
to `/tmp/fake.log` and `/tmp/serve.log`.

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
- Bounded buffering, one mechanism per side of the playhead. Behind it,
  `trimPlan()` removes (keeps ~3 s). Ahead of it, nothing is removed —
  `liveEdgeSeek()` jumps the playhead to the live edge when the buffered tail
  runs more than `TRIM_TAIL_CAP_S` (10 s) ahead, and what it passes becomes
  behind-side audio and is trimmed next pass. Removing ahead instead of
  jumping starves the playhead whenever the producer is faster than real time
  (measured: silent for the rest of a run, `tests/web/web_latency_test.mjs`).
  Trim and append must not interleave on one pass (`InvalidStateError`) —
  `_trimIfNeeded()` reports whether it trimmed and the loop `continue`s.
- Skipped audio, not stalled audio: the server drops a slow client's oldest
  chunks, which leaves holes in the buffered timeline, and MediaSource stops
  the playhead at a hole instead of noticing — the label reads "playing" over
  silence. `gapSkip()` (pure) is consulted on every append-loop pass: at a
  range tail with a later range behind it, inside a hole, or behind the buffer
  head, it seeks forward. At the end of the *last* range waiting is correct —
  that is the live edge, not a gap.
- Late joiners: one capture generation serves all `Listen` subscribers,
  and its cluster timecodes are absolute from generation start. A client
  that joins mid-generation would sit with its playhead at 0 and nothing
  buffered there — stalled and silent forever while the label says
  "playing". After the first committed append, `_seekIfNeeded()` jumps the
  playhead to `buffered.start(0)` (decision in pure `lateJoinSeek()`),
  once per generation; a join at the head of the generation needs no seek.
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
  functions. One `state` object in `app.js`; views are create-once +
  update-in-place modules (`createMonitor`/`createBank` build their DOM
  once per tab switch, then only write changed text/classes — never
  replacing nodes on the 250 ms status tick, so a pointer press can
  never straddle a node replacement and lose its click); keys and
  pointer events funnel into the same action functions.
- protobuf-es v2 plain objects are **camelCase** (`signalDetected`,
  `channelName`); the server sends display-format frequencies — normalize
  with `fromUserInput()` on receipt, send raw 8-digit strings.
- `stripPrefix()` removes the scanner's `CMD,` echo from raw responses
  (`MDL,UBC125XLT` → `UBC125XLT`).
- The stack script runs the prebuilt **debug** binary. rust-embed (no
  `debug-embed` feature) reads `dist/` from disk at runtime in debug
  builds, so web changes need only a browser reload — but release builds
  embed `dist/` at compile time and need `cargo build`.
- Focus events are suppressed while the browser window is unfocused, so
  modal field highlighting is driven explicitly (`setActive`), not by
  focus/blur alone.
