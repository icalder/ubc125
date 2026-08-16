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

### 1.3 Keyboard parity

The browser must honor the console keybindings (when not typing in a
field): ←/→ tabs, ↑/↓/j/k rows, 1–0 bank toggle, `p`/Space scan,
`m`/`W` hold, `e` edit, `d` delete, `Enter` save, `Esc` cancel/close.
`q` does not close the browser tab — repurpose as "back to Monitor".

---

## 2. Key technical decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Vite + React + TypeScript** in `web/` | Small, standard, maintainable. No UI kit — the terminal look is custom CSS (a component library would fight the design). |
| D2 | **`@grpc/web`** for transport | Required by the task. Server side is already wired: `TonicWeb` layer + CORS in `serve.rs`. |
| D3 | **Same-port serving, embedded static files**: embed `web/dist` at compile time (`staticdir`/`rust-embed`), serve via an axum/tonic fallback route | Nix-installed binaries have no predictable CWD; embedding makes the release binary self-contained and keeps one port. `dist/` is **committed** (same policy as generated proto code) — no node toolchain needed in the Nix build. |
| D4 | **New `ListChannels` RPC** (`uint32 bank → repeated Channel`) | The bank tab needs 50 channels; 50 sequential `GetChannel` round-trips over HTTP would each do a PRG/EPG mode dance. One RPC = one program-mode session, batched CIN reads. Maps directly onto the console's fetch-queue logic (which already lives in the client as `get_channel`; add a `get_bank_channels(bank)` typed op). |
| D5 | **Modulation: display only, v1 is AM-only edits** | Console edit flow is AM-only; keep parity, document. (Modulation is shown in the table and could become a field later.) |
| D6 | **Volume/squelch: display only in v1** | Console TUI shows them but editing is out of scope; the RPCs exist — stretch goal. |
| D7 | **Single `GetStatus` stream per client** | Server cancels a previous poller on a new `GetStatus` (already implemented). One UI = one stream; on reconnect the old poller is cleaned up server-side. |

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

- `npm create vite@latest web -- --template react-ts`; deps: `@grpc/web`,
  `@grpc/proto-loader` not needed — use generated TS (see 4.2).
- Generate TS client stubs from `services.proto`
  (`@bufbuild/protobuf` + `protoc-gen-es` or `@grpc-web` codegen — pick one
  that produces a browser `XhrTransport` client; keep the generator command
  in `web/README.md` like the Rust regen path).

### 4.2 Structure (SOLID: one component per box, logic in hooks)

```
web/src/
  main.tsx                 # mounts <App/>, global keyboard handler
  app.tsx                  # tab state (Monitor | Bank 1..10), view switch
  theme.css                # the whole look: colors, boxes, tab bar, table
  rpc/client.ts            # grpc-web clients (ScannerControl, SystemInfo)
  hooks/
    useScannerInfo.ts      # one-shot: model/version/audio settings
    useStatusStream.ts     # GetStatus stream, reconnect w/ backoff, last-good state
    useBanks.ts            # bank mask, toggle(bank) optimistic + rollback
    useBankChannels.ts     # ListChannels(bank), cache per bank, edit/delete ops
  views/
    monitor.tsx            # ScannerInfo + LiveScan + ActiveBanks boxes
    bank.tsx               # ChannelTable + cursor + edit modal + delete confirm
  components/
    box.tsx                # titled bordered box (the ratatui Block equivalent)
    tab-bar.tsx
    help-bar.tsx
    channel-table.tsx
    edit-channel-modal.tsx
    confirm.tsx
```

- `useStatusStream`: opens `GetStatus` on mount; updates state per message;
  on error → "SCANNER OFFLINE" banner (red, on the Live Scan box border),
  reconnect with exponential backoff (1s → 30s cap); keep last good values
  on screen (a serial hiccup must not blank the UI — same policy as the
  server stream).
- `useBanks.toggle`: optimistic flip → `SetEnabledBanks` → rollback +
  toast on error.
- Edit modal: local state seeded from the row; `Enter` → `SetChannel` →
  success blip or inline error (shows the gRPC status message, e.g.
  "invalid frequency"); `Esc`/backdrop → cancel. Frequency field accepts
  `123.9750` / `123.975` (server validates via `Frequency`).
- Delete: confirm dialog → `DeleteChannel` → clear row.
- Errors: a thin status strip (inside the help bar's right side) shows
  `unavailable` as `SCANNER OFFLINE`, other errors as the status message.

### 4.3 Look-and-feel implementation notes

- One CSS file, CSS custom properties for the palette (§1 table).
- `box.tsx`: 1px solid border; title rendered as text with a black
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

- `npm run build` → `web/dist/` (committed, per D3).
- Root: embed dist in the server (3.2). `just build-web` / npm scripts:
  `build` (vite), `dev` (vite dev server + `serve` with CORS for dev).
- Dev workflow: `vite dev` on 5173 proxies nothing — point the app at
  `127.0.0.1:50051` via an env var; CORS is already permissive.

---

## 5. Testing

| # | What | How |
|---|---|---|
| W1 | `get_bank_channels` typed op | mock-transport unit tests (command bytes, one mode transition, empty slots) |
| W2 | `ListChannels` RPC | add to `fake_e2e.sh` (grpcurl) and `hw_e2e.sh` |
| W3 | Static serving | `curl -s localhost:PORT/` in both e2e scripts returns index.html; gRPC paths unaffected |
| W4 | Pure client logic | vitest: frequency formatting/parsing passed to the server, bank mask → `[n]` states, backoff sequence |
| W5 | **Browser E2E against the fake scanner** (no hardware needed): scripted Chrome session via the `browser-tools` skill — load `/`, see model info; Live Scan updates from the stream; toggle a bank (verify fake received SCG write); open Bank 1, table populated from `ListChannels`; edit modal: change name, save, verify via `GetChannel`; delete a slot, verify row cleared; stream survives a fake "hiccup" (fake stops answering GLG for 3s, banner appears, then recovers) | `tests/web_e2e.md` with the exact scripted steps (run with browser-tools; not automated in CI yet) |
| W6 | **T6 hardware pass** (real browser, real scanner): same list as W5 minus the hiccup simulation; round-trip edits only (no destructive deletes beyond restoring) | manual, recorded in PLAN status |

---

## 6. Phases

**Phase A — server: `ListChannels` + static serving** (~half a day)
Proto change, typed client op, server impl, embedding + routing,
e2e-script updates. Exit: `cargo test` green, clippy clean,
`fake_e2e.sh` 20+/20+, `curl /` works.

**Phase B — web scaffold + theme + Monitor view** (~half a day)
Scaffold, codegen, `theme.css`, `box`/`tab-bar`/`help-bar`, hooks
(useScannerInfo, useStatusStream, useBanks), Monitor view. Exit: Monitor
view visually matches `main-console-screen.png` (side-by-side in browser
against the fake scanner); stream updates live.

**Phase C — Bank view: table, edit, delete** (~half a day)
useBankChannels, ChannelTable, edit modal, delete confirm, keyboard
parity. Exit: matches `bank1-screen.png` / `edit-frequency.png`; edit and
delete round-trips verified against the fake scanner.

**Phase D — polish + browser E2E + hardware**
W5 scripted pass, W6 hardware pass, `web/README.md`, AGENTS.md update
(describe web/ + regen paths), commit dist, final `PLAN` status line.
Exit: W5 + W6 recorded as passed; repo clean; single-port binary serves
both UI and gRPC.

---

## 7. Deliverables checklist

- [ ] `ListChannels` RPC (proto + client op + server + tests W1/W2)
- [ ] Embedded static serving on the gRPC port (W3)
- [ ] `web/` Vite+React+TS app, grpc-web client, committed `web/dist`
- [ ] Monitor view matching the console (info, live amber scan, bank toggles, scan/hold keys)
- [ ] Bank views matching the console (50-row table, cursor, edit modal, delete confirm)
- [ ] Keyboard parity (tabs, j/k, 1–0, p/m, e/d, Enter/Esc)
- [ ] Offline banner + stream reconnect with backoff
- [ ] W1–W6 all green (W5 via browser-tools, W6 on hardware)
- [ ] AGENTS.md + README updated; dist committed; nix flake build still clean

## 8. Open questions (decide at the start of each phase, not before)

- Codegen tool for TS stubs (`@bufbuild/protobuf` vs `protoc` +
  `grpc-web` npm codegen) — pick whichever yields the cleanest browser
  `XhrTransport` client; record the command.
- `q`/quit mapping in the browser (proposal: back to Monitor).
- Whether the Live Scan box stays amber when `signal_detected` is false
  (console keeps it highlighted — match console).
- Stretch (post-plan): volume/squelch sliders, modulation field in the
  edit modal, multiple concurrent browser clients.
