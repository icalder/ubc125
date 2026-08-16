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
    components/    # box, tabbar, helpbar, modal (titled ratatui-style boxes)
    views/         # monitor.js, bank.js — one module per view
    proto/         # generated protobuf-es JS (committed)
    tests/         # node --test unit tests for the pure logic
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

Debug builds serve `dist/` from disk at runtime (rust-embed debug mode),
so web changes only need a page reload — no rebuild, no restart.

## Tests

```sh
cd web && node --test dist/tests/*.test.js
```

Covers the pure logic (`freq.js`): frequency parsing/normalization and
display formatting. Browser-level verification is scripted in
`tests/web_e2e.md` (run with the browser-tools skill against the fake
scanner) — not part of `node --test`.

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
- Focus events are suppressed while the browser window is unfocused, so
  modal field highlighting is driven explicitly (`setActive`), not by
  focus/blur alone.
