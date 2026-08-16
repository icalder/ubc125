# UBC125 — Web UI Delivery Plan

Scope: browser UI for the core scanning features, talking to the existing
gRPC server over **grpc-web**. The UI **mimics the console look-and-feel**
(see `main-console-screen.png`, `bank1-screen.png`, `edit-frequency.png` at
repo root). Everything else (phases 1–4: typed client, console, build
hygiene, gRPC service) is complete and verified; this plan assumes it.

Reference: the console TUI is the source of truth for layout, content and
keybindings. The Web UI is a faithful translation to the browser, not a
re-invention.

---

## 0. Context for a new session

Project: Rust control program for the Uniden UBC125XLT scanner, driven over
a USB serial port (115200 baud, raw, `\r`-terminated commands). One binary,
two modes: `console` (ratatui TUI) and `serve` (gRPC + grpc-web server).

### Key files

| Path | What |
|---|---|
| `src/scanner.rs` | `ScannerClient` — ALL serial protocol. `Transport` trait (`SerialTransport` prod; `MockTransport` in `#[cfg(test)] pub(crate) mod mock`), `ScannerError` (thiserror), ~15 typed ops, PRG/MON mode state owned here |
| `src/server.rs` | gRPC impl (`ScannerServer`), `with_scanner` helper, `From<ScannerError> for Status`, `GetStatus` poller stream |
| `src/cmd/serve.rs` | `serve` command: tonic server, `TonicWeb` layer + CORS, reflection |
| `src/cmd/console.rs` | TUI (ratatui) — the look-and-feel reference for the Web UI |
| `src/types.rs` | Domain types (`Channel`, `ChannelIndex`, `Frequency`, `BankMask`, `ScanStatus`, `Modulation`) — well tested |
| `src/constants.rs` | `NUM_BANKS`, `POLL_INTERVAL_MS`, bounds |
| `lib/grpc/proto/ubc125/v1/services.proto` | The gRPC contract |
| `lib/grpc/rust-gen/` | Generated prost/tonic code, **committed** (package `ubc125-grpc`) |
| `tests/fake_scanner.py` + `tests/fake_e2e.sh` | Fake scanner on a socat pty pair + full grpcurl matrix (no hardware needed) |
| `tests/hw_e2e.sh` | Same matrix against the real scanner — **non-destructive** (round-trips only) |
| `SCANNER-COMMANDS.md` | Serial protocol reference (documented + reverse-engineered) |

### Build & test commands

```sh
cargo build                 # host build (toolchain pinned via rust-toolchain.toml)
cargo test                  # ~80 unit tests, all offline (mock transport)
cargo clippy --all-targets  # must stay clean
nix flake check             # cross-compile verification (x86_64 + aarch64)

# gRPC e2e, no hardware (fake scanner):
nix-shell -p socat grpcurl --run 'bash tests/fake_e2e.sh'      # ~18 checks
# gRPC e2e, real scanner (attach first; non-destructive):
nix-shell -p grpcurl --run 'bash tests/hw_e2e.sh'              # 20 checks
# socat is available for raw protocol spot-checks:
echo -ne "MDL\r" | socat -t 1 - /dev/ttyACM0,b115200,raw,echo=0
```

NixOS host: anything not installed can be pulled with `nix-shell -p <pkg>`.

### Conventions

- SOLID/clean-code discipline; the typed `ScannerClient` ops are the only
  place raw command strings live — new RPCs must go through them.
- Errors: `ScannerError` at the client, `tonic::Status` at the server
  (validation → `invalid_argument`, serial/timeout → `unavailable`,
  protocol surprise → `internal`).
- Committed generated code: after editing `services.proto` run
  `UBC125_REGEN=1 cargo build -p ubc125-grpc` and commit the updated
  `lib/grpc/rust-gen/src/proto/` files.
- **Never do destructive things to the real scanner** without explicit user
  approval: no deleting user channels, no changing bank states or channel
  data. Round-trip writes of the exact values just read are OK (that is
  what `tests/hw_e2e.sh` does).
- Hardware: `/dev/ttyACM0` when attached; it may not be attached on a given
  day — the fake-scanner e2e is the default verification path.

### Status before this plan

- All gRPC RPCs implemented and verified (unit + fake e2e + real-hardware
  T5 20/20 + 10-min T7 soak). ~80 tests green, clippy clean.
- Nothing for the Web UI exists yet. `web/` does not exist.

---

## 1. What the UI must look like (from the console screenshots)

Global chrome (both views):

- Black background, monospaced font, light-gray/white text, thin white
  1px borders around titled boxes (ratatui `Block` style: the title sits
  on the top border line).
- **Tab bar** (top, boxed): `Monitor | Bank 1 | Bank 2 | … | Bank 10`.
  Active tab: bold white. Inactive: dim gray.
- **Help bar** (bottom, boxed): per-view key hints, exactly like the
  console ("Use Left/Right to switch tabs. Up/Down or j/k to navigate.
  'e': Edit, 'd': Delete, 'q': Quit." etc.).

Colors (approximate from screenshots):

| Element | Color |
|---|---|
| background | `#000000` |
| text / borders | `#e0e0e0` / white |
| live scan highlight (box fill) | amber `#ffb000`, black text |
| enabled bank `[1]` `[2]` | green `#00c000` |
| disabled bank | dim gray |
| cursor row in channel table | inverted (white fill, black text) + `>>` marker |
| edit-modal frequency field | amber border + amber text |
| edit-modal name field | white border, white text |

### 1.1 Monitor view (default)

Stacked boxes:

1. **Scanner Info** — `Model:`, `Version:`, `Volume:`, `Squelch:` rows.
2. **Live Scan** — amber-filled box: `Bank:`, `Frequency: <x> MHz`,
   `Channel:` rows. Updates in real time from the `GetStatus` stream.
   When no transmission is detected the box shows the idle state (console
   shows the last/empty GLG state — match that).
3. **Active Banks (Press 1–0 to toggle)** — `Banks: [1] [2] … [10]`;
   click toggles (also keys 1–0, 0 = bank 10, like the console).
4. Help bar: scan/hold hints (`p`/Space: Scan, `m`/W: Hold).

### 1.2 Bank view (×10)

1. Boxed table, columns `Idx | Name | Freq | Mod`, 50 rows. Empty slots
   render as blank rows (the console prints `Auto` in the Mod column for
   empties — replicate that quirk).
2. Row navigation: Up/Down, `j`/`k`, or click. Cursor row inverted with
   `>>` prefix.
3. **Edit Channel modal** (overlay, boxed, centered) — opened with `e` or
   a row action: `Frequency (MHz)` field (amber), `Name` field (white),
   footer `Enter: Save | Esc: Cancel`, `Tab` switches fields.
4. Delete with `d` (or row action) — confirmation prompt (console deletes
   straight away; the Web adds a confirm because a mistyped key is
   costlier in a browser).
5. Help bar: navigate/edit/delete/quit hints.

### 1.3 Interaction: keys AND pointer/touch (phones are first-class)

The terminal works because arrow keys are free; the browser must make
**every** action available by key *and* by tap/click, with no keyboard
required at any point. Touch targets ≥ 44 px.

| Action | Keys | Pointer / touch |
|---|---|---|
| Switch tab | ←/→ | tap the tab |
| Select channel row | ↑/↓, j/k | tap the row (cursor follows) |
| Toggle bank | 1–0 | tap the `[n]` |
| Scan / Hold | `p`/Space, `m`/`W` | **Scan** / **Hold** buttons in the bottom bar |
| Edit channel | `e` | tap row, then **Edit** button in the bottom bar (or a per-row ✎ affordance) |
| Delete channel | `d` | tap row, then **Delete** button → confirm dialog with real **Yes/No** buttons |
| Save / cancel modal | Enter / Esc | **Save** / **Cancel** buttons in the modal footer |
| Back to Monitor | `q` | tap the `Monitor` tab |

Implementation consequences:

- The **help bar doubles as an action bar**: bottom-anchored (thumb
  reach), context-sensitive buttons next to the key hints — Monitor view
  shows Scan/Hold; Bank view shows Edit/Delete for the selected row
  (disabled when the row is empty). Buttons are real `<button>` elements,
  keyboard still works as a parallel input path.
- Modal fields are real focusable `<input>`s — tapping one opens the
  phone's keyboard; the modal footer keeps visible Save/Cancel so no
  Enter key is needed.
- Row selection is tap-to-move-cursor, not tap-to-edit: two-step
  (select, then act) matches the console model and avoids fat-finger
  edits.

### 1.4 Responsive layout (phone portrait → desktop)

- Single fluid column, same box stack; no side-by-side desktop layout to
  maintain.
- **Tab bar**: 11 tabs don't fit a 390 px screen — horizontal scroll with
  the active tab auto-scrolled into view (sticky), or two wrapped rows;
  decide at Phase B by eyeballing in a 390×844 emulation.
- **Channel table**: fixed layout, `Name` column absorbs slack and
  truncates with ellipsis; `Idx`/`Freq`/`Mod` keep tabular widths; 50 rows
  scroll inside the box on short screens (the cursor row scrolls into
  view on selection).
- Font: `clamp()`-based sizing so the terminal density survives both a
  phone and a 1080p monitor.
- All of the above must hold at 390×844 (phone portrait) and 1280×720
  (the console screenshots' size).

---

## 2. Key technical decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Vanilla ES modules + plain CSS** in `web/` (no framework, no bundler in the critical path) | The app is two views, one modal, one 50-row table and a small state object — a direct translation of the console's state machine. No React re-render semantics to fight the terminal aesthetic, no Vite toolchain, no node dependency in the Nix flake. `@grpc/web` + generated JS are ESM packages, consumed via **import maps** (zero build step; CDN URLs pinned in `web/dist/index.html`) or a one-line `esbuild` bundle if offline use is required. Decision D8 settles which. |
| D2 | **`@grpc/web`** for transport | Required by the task. Server side is already wired: `TonicWeb` layer + CORS in `serve.rs`. |
| D3 | **Same-port serving, embedded static files**: embed `web/dist` at compile time (`staticdir`/`rust-embed`), serve via an axum/tonic fallback route | Nix-installed binaries have no predictable CWD; embedding makes the release binary self-contained and keeps one port. `dist/` is **committed** (same policy as generated proto code) — no node toolchain needed in the Nix build. |
| D4 | **New `ListChannels` RPC** (`uint32 bank → repeated Channel`) | The bank tab needs 50 channels; 50 sequential `GetChannel` round-trips over HTTP would each do a PRG/EPG mode dance. One RPC = one program-mode session, batched CIN reads. Maps directly onto the console's fetch-queue logic (which already lives in the client as `get_channel`; add a `get_bank_channels(bank)` typed op). |
| D5 | **Modulation: display only, v1 is AM-only edits** | Console edit flow is AM-only; keep parity, document. (Modulation is shown in the table and could become a field later.) |
| D6 | **Volume/squelch: display only in v1** | Console TUI shows them but editing is out of scope; the RPCs exist — stretch goal. |
| D7 | **Single `GetStatus` stream per client** | Server cancels a previous poller on a new `GetStatus` (already implemented). One UI = one stream; on reconnect the old poller is cleaned up server-side. |
| D9 | **Pointer/touch is a first-class input path** (§1.3–1.4) | The UI must be fully usable on a phone: every key action has a tap/click equivalent; the help bar doubles as a context-sensitive action bar (Scan/Hold, Edit/Delete); modals have real buttons and focusable inputs; layout is responsive from 390 px to desktop. Keys remain a parallel path, not the only one. |
| D8 | **Dependency consumption: import maps (default) vs esbuild (fallback)** | Default: `web/dist` is fully static — `index.html` + `app.js` + `theme.css` + `proto/*.js` (generated), with an import map pointing `@grpc/web` and `@bufbuild/protobuf` at pinned jsdelivr ESM URLs. No build step at all; committing `dist/` is committing the app. Fallback (pick this if the UI must work offline/intranet): vendor the two packages into `web/dist/vendor/` via a single `esbuild --bundle` command documented in `web/README.md`. Both keep the Nix flake node-free. Decide when scaffolding (Phase B); record the choice in `web/README.md`. |

---

## 3. Server work (Rust)

### 3.1 `ListChannels` RPC

- `services.proto`:
  ```proto
  rpc ListChannels(ListChannelsRequest) returns (ListChannelsResponse);
  message ListChannelsRequest { uint32 bank = 1; }   // 1..=10
  message ListChannelsResponse { repeated Channel channels = 1; } // ≤50
  ```
- Client: `ScannerClient::get_bank_channels(bank) -> Result<Vec<Channel>>` —
  one `ensure_program`, 50 `CIN` reads (skip-continue on per-slot timeout,
  like the console fetch queue), one return-to-monitor. Reuse
  `get_channel` internally.
- `server.rs`: validate bank 1..=10 (`invalid_argument`), `with_scanner`,
  map `Vec<Channel>` → proto `Channel`.
- Regenerate proto code (`UBC125_REGEN=1 cargo build`), update
  `tests/fake_scanner.py` (answer CINs for the batch) and
  `tests/fake_e2e.sh` + `tests/hw_e2e.sh` with `ListChannels` checks.
- Unit tests (mock transport): command byte order (PRG, 50×CIN, EPG/KEY),
  empty-bank handling, one mode transition for the whole batch.

### 3.2 Static file serving

- Embed `web/dist` (start with a placeholder `index.html` so the server
  work is testable before the app exists).
- Routing: gRPC paths (`/ubc125.v1.*`) hit tonic; everything else serves a
  file from the embedded dir, `/` → `index.html`, missing file → 404.
- Verify grpc-web responses still pass through (TonicWeb layer is applied
  before the static fallback).
- Tests: `hw_e2e.sh`/`fake_e2e.sh` gain a `curl /` check (200 +
  `index.html`); a unit/integration test that `/ubc125.v1...` is not
  shadowed by the static layer.

---

## 4. Web client work (`web/`)

### 4.1 Scaffold

- No framework, no scaffold tool: `web/src/` is plain ES modules + one CSS
  file; `web/dist/` is what gets committed and embedded (for the import-map
  option, `dist/` ≈ `src/` — a copy or symlink-free duplication kept in
  sync by a trivial `just`/npm script, or develop directly in `dist/`;
  decide at scaffold time and record in `web/README.md`).
- Proto codegen for the browser: `protoc --plugin=protoc-gen-es` (or
  `@bufbuild/protoc-gen-es`) with `--target=js` against `services.proto`,
  output committed under `web/src/proto/`. Record the exact command in
  `web/README.md` (mirror of the Rust `UBC125_REGEN` convention).
- The grpc-web client is `@grpc/web`'s `XhrTransport` + the generated
  service client; wrap it in `rpc/client.js` so the rest of the app never
  touches transport details.
- No TypeScript required (a ~1–1.5k-line app); JSDoc on the non-obvious
  functions is enough.

### 4.2 Structure (SOLID: one module per box, state in one place)

One `state` object + a `render()` per view; no virtual DOM, no
subscriptions. DOM is static after initial build — updates patch text
nodes / classes / row cells.

```
web/src/
  index.html               # layout skeleton + import map (if D8=import maps)
  app.js                   # state object, tab switching, global keymap
  theme.css                # the whole look: colors, boxes, tab bar, table
  rpc/client.js            # grpc-web clients (ScannerControl, SystemInfo)
  proto/                   # generated JS (committed), from services.proto
  views/
    monitor.js             # ScannerInfo + LiveScan + ActiveBanks boxes
    bank.js                # ChannelTable + cursor + edit modal + delete confirm
  components/
    box.js                 # titled bordered box (the ratatui Block equivalent)
    tab-bar.js
    help-bar.js
    channel-table.js
    edit-channel-modal.js
    confirm.js
  logic/
    status-stream.js       # GetStatus loop, reconnect w/ backoff, last-good state
    banks.js               # bank mask state, toggle(bank) optimistic + rollback
    channels.js            # ListChannels cache per bank, edit/delete ops
    freq.js                # display formatting / input normalization (unit-tested)
  tests/
    freq.test.js           # node --test
```

- `status-stream.js`: opens `GetStatus` on load; updates state per message
  and re-renders the Live Scan box; on error → "SCANNER OFFLINE" banner
  (red, on the Live Scan box border), reconnect with exponential backoff
  (1s → 30s cap); keep last good values on screen (a serial hiccup must
  not blank the UI — same policy as the server stream).
- `banks.js` toggle: optimistic flip → `SetEnabledBanks` → rollback +
  inline error on failure.
- Edit modal: local fields seeded from the row (real focusable `<input>`s);
  Save button or `Enter` → `SetChannel` → success blip or inline error
  (shows the gRPC status message, e.g. "invalid frequency"); Cancel
  button, `Esc` or backdrop → cancel. Frequency field accepts `123.9750` /
  `123.975` (server validates via `Frequency`).
- Delete: confirm dialog with Yes/No buttons (keys: `d`/Enter/Esc) →
  `DeleteChannel` → clear row.
- Global keymap (`app.js`): one `keydown` handler, a switch on the active
  view + focus (ignore keys while a text input is focused, except Esc),
  mirroring the console table in §1.3. Keys and pointer events funnel into
  the **same action functions** (`actions.scan()`, `actions.toggleBank(n)`,
  `actions.selectRow(i)`, `actions.edit()`, …) — one behavior source, two
  input paths (D9).
- Action bar: the help-bar component renders context buttons
  (Monitor: Scan/Hold; Bank: Edit/Delete for the selected row) as real
  `<button>`s wired to the same action functions; Edit/Delete disabled on
  empty rows.
- Errors: a thin status strip (right side of the help bar) shows
  `unavailable` as `SCANNER OFFLINE`, other errors as the status message.

### 4.3 Look-and-feel implementation notes

- One CSS file, CSS custom properties for the palette (§1 table).
- `box.js`: 1px solid border; title rendered as text with a black
  background span overlapping the top border (the ratatui effect).
- Tabular numbers (`font-variant-numeric: tabular-nums`) so the Freq column
  doesn't jitter as the stream updates.
- No scrollbars on the 50-row table (50 rows fit at ≤14px font on a
  typical screen; if not, the box scrolls internally — match console
  density).
- Cursor row: `background: #e0e0e0; color: #000;` + `>>` in the Idx cell.
- Live Scan box: `background: #ffb000; color: #000;` while a transmission
  is present (signal_detected), dim amber otherwise.

### 4.4 Build & wiring

- Per D3/D8: `web/dist/` is committed static files; the server embeds it.
  Import-map option: no build step (edit in `dist/` or sync from `src/` —
  record the choice). Esbuild option: one documented bundle command.
- Root: embed dist in the server (§3.2).
- Dev workflow: run `ubc125 serve` (or against the fake scanner via
  `tests/fake_e2e.sh`'s setup) and open the UI in a browser — same port,
  no dev server. The server address the app talks to is
  `location.origin` by default, overridable via `?server=host:port` for
  cross-origin dev (CORS is already permissive).

---

## 5. Testing

| # | What | How |
|---|---|---|
| W1 | `get_bank_channels` typed op | mock-transport unit tests (command bytes, one mode transition, empty slots) |
| W2 | `ListChannels` RPC | add to `fake_e2e.sh` (grpcurl) and `hw_e2e.sh` |
| W3 | Static serving | `curl -s localhost:PORT/` in both e2e scripts returns index.html; gRPC paths unaffected |
| W4 | Pure client logic | `node --test web/src/tests/` (node is available via `nix-shell -p nodejs`): frequency display/normalization, bank mask → `[n]` states, backoff sequence |
| W5 | **Browser E2E against the fake scanner** (no hardware needed): scripted Chrome session via the `browser-tools` skill, at **both** 1280×720 and a 390×844 phone emulation, **all interactions via CDP click/tap** (the pointer path; the key path is parallel by construction) — load `/`, see model info; Live Scan updates from the stream; tap-toggle a bank (verify fake received SCG write); tap **Scan**/**Hold**; open Bank 1, table populated from `ListChannels`; tap a row → **Edit** → change name → **Save**, verify via `GetChannel`; select → **Delete** → **Yes**, verify row cleared; stream survives a fake "hiccup" (fake stops answering GLG for 3s, banner appears, then recovers) | `tests/web_e2e.md` with the exact scripted steps (run with browser-tools; not automated in CI yet) |
| W6 | **T6 hardware pass** (real browser, real scanner): same list as W5 minus the hiccup simulation; round-trip edits only (no destructive deletes beyond restoring) | manual, recorded in PLAN status |

---

## 6. Phases

**Phase A — server: `ListChannels` + static serving** (~half a day)
Proto change, typed client op, server impl, embedding + routing,
e2e-script updates. Exit: `cargo test` green, clippy clean,
`fake_e2e.sh` 20+/20+, `curl /` works.

**Phase B — web scaffold + theme + Monitor view** (~half a day)
Scaffold `web/` per D8, proto codegen, `theme.css`, `box`/`tab-bar`/
`help-bar` (with action buttons) components, `status-stream.js` +
`banks.js`, Monitor view. Exit: Monitor view visually matches
`main-console-screen.png` (side-by-side in a browser against the fake
scanner) at 1280×720 **and** 390×844; stream updates live; Scan/Hold and
tap-to-toggle banks work by pointer; `W4` tests run under `node --test`.

**Phase C — Bank view: table, edit, delete** (~half a day)
`channels.js`, `channel-table.js` (tap-to-select, scroll-into-view), edit
modal (buttons + inputs), delete confirm, global keymap as parallel input.
Exit: matches `bank1-screen.png` / `edit-frequency.png` at both viewport
sizes; the full select→edit→save and select→delete→confirm flows work
**entirely by tap**; edit and delete round-trips verified against the fake
scanner.

**Phase D — polish + browser E2E + hardware**
W5 scripted pass, W6 hardware pass, `web/README.md` (codegen + build +
dev commands), AGENTS.md update (describe `web/` + regen paths), dist
committed, final PLAN status line.
Exit: W5 + W6 recorded as passed; repo clean; single-port binary serves
both UI and gRPC.

---

## 7. Deliverables checklist

- [ ] `ListChannels` RPC (proto + client op + server + tests W1/W2)
- [ ] Embedded static serving on the gRPC port (W3)
- [ ] `web/` vanilla-ESM app (no framework), grpc-web client, committed `web/dist`
- [ ] Monitor view matching the console (info, live amber scan, bank toggles, scan/hold)
- [ ] Bank views matching the console (50-row table, cursor, edit modal, delete confirm)
- [ ] Every action usable by pointer/touch **and** keys (D9); action bar; 44 px targets
- [ ] Responsive at 390×844 and 1280×720
- [ ] Offline banner + stream reconnect with backoff
- [ ] W1–W6 all green (W5 via browser-tools, W6 on hardware)
- [ ] AGENTS.md + README updated; dist committed; nix flake build still clean

## 8. Open questions (decide at the start of each phase, not before)

- D8: import maps (CDN) vs esbuild (vendored) — default import maps;
  switch only if offline use is required. Record in `web/README.md`.
- Proto codegen specifics: `protoc-gen-es` flags for JS output + the
  `@grpc/web` client wrapper — verify a minimal GetModelInfo call works
  before building the views (first task of Phase B).
- `q`/quit mapping in the browser (proposal: back to Monitor).
- Whether the Live Scan box stays amber when `signal_detected` is false
  (console keeps it highlighted — match console).
- Stretch (post-plan): volume/squelch sliders, modulation field in the
  edit modal, multiple concurrent browser clients.
