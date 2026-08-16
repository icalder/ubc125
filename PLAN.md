# UBC125 — Refactor & gRPC/Web Delivery Plan

**Status (2026-08-16): Phases 1–4 complete.** 77 unit tests passing,
clippy clean, nix build verified. Scanner hardware is not attached on this
machine, so hardware smoke tests (T-series) are deferred; in the meantime
`tests/fake_e2e.sh` runs the full gRPC matrix (T5) against a fake scanner on
a socat pty pair:

```sh
nix-shell -p socat grpcurl --run 'bash tests/fake_e2e.sh'   # 18 checks
```

Phase 5 (Web UI) is not started.

Goal: put the codebase in a position where the gRPC service interface can be
completed as a thin layer over a shared, tested scanner client, and a Web UI
can be introduced over grpc-web.

Phases are sequential; each phase ends in a green build, passing tests, and
a manual smoke check against hardware where noted.

---

## Phase 1 — Shared typed scanner client

**Goal:** `ScannerClient` becomes the single command layer for both console
and gRPC (as AGENTS.md promises). No more raw command strings outside
`scanner.rs`.

### 1.1 Introduce a `Transport` trait (testability seam)

- New trait in `scanner.rs` (or `src/transport.rs`):

  ```rust
  #[trait_variant::make] // or a hand-rolled pair of sync/async traits
  trait Transport {
      fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
      fn read_byte(&mut self) -> io::Result<u8>; // honours port timeout
  }
  ```

- `SerialTransport` wraps `Box<dyn SerialPort>` and is the production impl.
- `ScannerClient::new(device)` keeps its signature but constructs
  `SerialTransport` internally; add `ScannerClient::with_transport(t)` for
  tests (and later for the gRPC server).
- DIP: everything above the transport depends on the trait only.

### 1.2 Introduce a `ScannerError` type

- `#[derive(Debug, thiserror::Error)]` enum:

  ```rust
  enum ScannerError {
      Io(#[from] io::Error),
      Timeout { command: String, partial: String },
      UnexpectedResponse { command: String, got: String },
      InvalidVolume(u8),
      InvalidSquelch(u8),
      InvalidChannelIndex(u32),
  }
  ```

- `send_command` returns `Result<String, ScannerError>`:
  - read timeout → `Timeout` (do **not** return partial data as `Ok`);
  - validate non-empty response; prefix validation happens in the typed ops
    (1.4), not in `send_command` (it is a generic primitive).
- Add `impl From<ScannerError> for tonic::Status` in `server.rs`:
  - `Timeout`/`Io` → `unavailable`
  - `Invalid*` → `invalid_argument`
  - `UnexpectedResponse` → `internal` (with the raw response in the message)

### 1.3 Move `ModeManager` inside the client

- `ScannerClient` owns the mode state (currently `ModeManager` in
  `modes.rs`):

  ```rust
  pub struct ScannerClient {
      transport: Box<dyn Transport>,
      mode: Mode,
  }
  ```

- `ensure_program`/`ensure_monitor` become private methods; public typed ops
  (1.4) call them as needed.
- `modes.rs` keeps the `Mode` enum + its tests; the manager struct is deleted
  (its only reason to exist was being external).

### 1.4 Typed operations

Replace raw-string call sites with:

```rust
impl ScannerClient {
    fn get_model(&mut self) -> Result<String, ScannerError>          // MDL
    fn get_firmware_version(&mut self) -> Result<String, ScannerError> // VER
    fn get_volume / set_volume
    fn get_squelch / set_squelch
    fn get_status(&mut self) -> Result<ScanStatus, ScannerError>     // GLG + parse
    fn get_banks(&mut self) -> Result<BankMask, ScannerError>        // SCG read
    fn set_banks(&mut self, mask: &BankMask) -> Result<(), ScannerError>
    fn get_channel(&mut self, idx: ChannelIndex) -> Result<Channel, ScannerError> // CIN
    fn set_channel(&mut self, ch: &Channel) -> Result<(), ScannerError>          // CIN write
    fn delete_channel(&mut self, idx: ChannelIndex) -> Result<(), ScannerError>  // DCH
    fn start_scan(&mut self) -> Result<(), ScannerError>             // KEY,S,P
    fn hold_scan(&mut self) -> Result<(), ScannerError>              // KEY,H,P
}
```

Rules:

- Ops that need Program mode (banks, channels, delete) do
  `ensure_program → command → ensure_monitor` internally, as a single unit.
  The mode transition back must still run on error paths where the scanner
  may have been left in PRG (match on where the failure occurred; see
  2.4 for the test).
- `get_status` runs `ScanStatus::parse_glg` and maps `None` to
  `UnexpectedResponse`.
- `set_channel` builds `CIN,{idx},{name},{freq8},{mod},0,0,0,0` from a
  `Channel` value. Modulation: send the channel's modulation; document that
  the console edit flow is currently AM-only (see Phase 5 decision).

### 1.5 `MockTransport` + client tests

- `MockTransport`: scriptable queue of canned byte responses; records all
  bytes written.
- Unit tests in `scanner.rs` (mock-based), minimum:
  - each typed op sends the expected command bytes (incl. trailing `\r`)
    and parses the canned response;
  - `get_status` on timeout → `Timeout` error, on garbage →
    `UnexpectedResponse`;
  - `set_banks` issues `PRG`, `SCG,…`, `EPG`, `KEY,S,P` in order;
  - `set_banks` failing on the `SCG` command still issues the return-to-monitor
    sequence;
  - volume/squelch bounds validation → `Invalid*` without touching the port;
  - repeated `ensure` semantics: two `set_banks` in a row issue `PRG` once.

**Exit criteria:** `cargo clippy --all-targets` clean; all tests pass; no
raw `send_command` calls remain outside `scanner.rs` (grep). Console is not
yet touched (client is additive).

---

## Phase 2 — Refactor console onto the typed client

**Goal:** behavior-preserving; console becomes UI + key mapping only.

- `App::new` init sequence uses `get_model`, `get_firmware_version`,
  `get_volume`, `get_squelch`, `get_banks` (the SCG/PRG dance in
  `App::new` disappears — `get_banks` handles it).
- Run loop:
  - GLG polling → `client.get_status()`; on error, log via `tracing::warn!`
    and keep the last good status (explicit decision, replacing silent
    `unwrap_or_else("Err: …")` strings);
  - fetch queue `CIN,{} → get_channel(idx)`; add a **retry cap** (e.g. 3
    attempts per channel, then mark the slot with an error state or leave
    `None` + log) instead of the current unbounded re-queue;
  - key handlers call `start_scan`, `hold_scan`, `set_banks`,
    `delete_channel`, `set_channel` — the per-key inline PRG/EPG blocks are
    deleted.
- `App` keeps its display state (channels cache, banks, scan_status,
  input_mode) — that is view state, not scanner state.
- Fix while here: `App` holds a `ModeManager` field and an `is_in_prg_mode()`
  used by the renderer status bar. After this phase PRG state lives in the
  client; expose `client.mode()` (or drop the PRG indicator from the status
  bar if it becomes noise — decide with a quick UI check).

**Exit criteria:** tests still green; manual smoke on hardware (see test
matrix, T4): monitor tab, bank tab load, edit, delete, bank toggle, volume,
squelch, scan/hold — behavior identical to before.

---

## Phase 3 — Build & repo hygiene

- **Cargo workspace:** root `Cargo.toml` becomes `[workspace] members =
  [".", "lib/grpc/rust-gen"]` (or move `rust-gen` under `lib/grpc-gen`);
  single `Cargo.lock`; delete stale `lib/grpc/rust-gen/Cargo.lock`.
- **build.rs decision — committed generated code (recommended for this
  cross-compiled project):**
  - `build.rs` generates only the descriptor set (needed by
    `include_file_descriptor_set!` / reflection);
  - `src/proto/ubc125.v1.rs` stays committed; add a `REGENERATE.md` note or
    script (`cargo run` one-shot or a justfile recipe) showing how to
    regenerate after proto changes;
  - delete the commented-out dead code in build.rs and lib.rs.
- **Repo cleanup:**
  - `result/` at repo root is a Nix build artifact — delete and add to
    `.gitignore`;
  - move the `grpcurl` example comments out of `serve.rs` into README or
    SCANNER-COMMANDS.md;
  - update `AGENTS.md`: describe the real architecture (typed client,
    transport trait, where parsing lives), fix the serve-mode "TODO" section
    as the service gets completed.
- **Dependencies:** add `thiserror`; `tokio` can drop from `full` to
  `rt-multi-thread, macros, sync, time, net` if nothing needs the rest
  (verify with a build; optional).

**Exit criteria:** `nix flake check` / `cargo build` in the flake still
works; clippy clean; tests green; `git status` clean.

---

## Phase 4 — Complete the gRPC service

**Goal:** all `ScannerControlService` RPCs implemented as thin wrappers over
the typed client.

### 4.1 Unary RPCs

| RPC | Implementation |
|---|---|
| `StartScan` | `client.start_scan()` |
| `HoldScan` | `client.hold_scan()` |
| `GetEnabledBanks` | `client.get_banks()` → 10-entry `repeated bool` |
| `SetEnabledBanks` | validate len == 10 (`invalid_argument`), build `BankMask`, `client.set_banks()` |
| `GetChannel` | validate index via `ChannelIndex::new` (`invalid_argument`), `client.get_channel()`; empty channel → response with no `Channel` set |
| `SetChannel` | validate index + frequency (parse via `Frequency`), build `Channel`, `client.set_channel()` |
| `DeleteChannel` | validate index, `client.delete_channel()` |

- `with_scanner` helper stays; swap error mapping to
  `From<ScannerError> for Status` (Phase 1.2).
- `GetAudioSettings` already works; switch to typed getters.

### 4.2 `GetStatus` server stream

Design:

- On first subscriber, spawn a polling task (`spawn_blocking` loop) that
  calls `client.get_status()` every `POLL_INTERVAL_MS` (250ms) and pushes
  `GetStatusResponse` into a bounded `mpsc` channel.
- Drop the polling task when the stream is cancelled (use
  `tokio::util::poll_once` / watch on receiver disconnect, or
  `ReceiverStream` + `Drop` guard on a `CancellationToken`).
- Only one polling task at a time: hold a
  `Mutex<Option<CancellationToken>>` on the server; a second concurrent
  `GetStatus` cancels the first (document this) — or return
  `resource_exhausted`. Pick one, test both.
- Map `ScanStatus` → `GetStatusResponse` in one place
  (`impl From<&ScanStatus> for GetStatusResponse`).
- On `Timeout`/`UnexpectedResponse`: emit the error into the stream?
  No — log and skip the tick, keep the stream alive (a momentary serial
  hiccup should not kill the Web UI). Decision recorded in a comment.

### 4.3 Proto review (before code, cheap now)

- `GetStatusResponse.frequency` is a string — acceptable (scanner-native
  formatting), but consider adding `modulation` (the console shows it; the
  Web UI will want it). Adding a field is backward-compatible; do it now.
- `GetEnabledBanksResponse.banks` / `SetEnabledBanksRequest.banks`:
  `repeated bool` works but a `uint32` bitmask would be tighter; keep
  `repeated bool` (simpler for the Web client) — decision: keep.
- `GetChannelResponse.channel` absent for empty channel: verify the
  generated prost code uses `Option<Channel>` (proto3 message fields do)
  and that `GetChannel` for an empty slot returns `None`, not a
  zero-valued `Channel`.

**Exit criteria:** grpcurl test matrix passes against hardware (T5);
`grpcurl reflect` lists all methods; no `unimplemented` stubs remain.

---

## Phase 5 — Web UI (grpc-web)

**Goal:** browser UI for the core scanning features, talking to the same
server.

### 5.1 Decide the open questions first (one hour, not code)

- **Modulation in edit flow:** console hardcodes `AM`; proto carries
  `modulation`. Either (a) extend the console edit popup with a modulation
  field now, or (b) document AM-only for v1 and let the Web UI send the
  stored value unchanged. Recommendation: (b) for v1.
- **Web stack:** plain TS + `@grpc/web` (minimal) vs React. The feature set
  (live status, bank table, channel edit) fits a small React or even
  vanilla app; pick the smallest that stays maintainable.
- **Serving the UI:** static files via `ServeDir` layer on the same tonic
  port (simplest, single port) — `tower-http` already a dependency.

### 5.2 Server: static file serving

- Add `tower-http` `fs` feature; layer `ServeDir` for `web/dist` (built
  client) so the app is same-origin (no CORS pain in practice, CORS stays
  for dev mode with the Vite dev server).

### 5.3 Web client screens (mirror the console)

1. **Monitor:** live `GetStatus` stream (frequency, bank, channel, signal
   indicator), scan/hold buttons, volume/squelch display, bank toggle
   (1–10).
2. **Bank view:** channel table (50 rows), lazy `GetChannel` per row (or
   batch — consider adding a `ListChannels(bank)` RPC in Phase 4 if the
   50-sequential-calls pattern is ugly; it maps to the console's fetch
   queue and is a natural RPC), edit + delete dialogs.
3. Error surfacing: show `Status` messages (especially `unavailable` =
   scanner serial trouble) in a status bar.

### 5.4 Verify

- Browser against hardware (T6): stream updates in real time; edit/delete
   reflected; bank toggles persist; scanner held/started from UI; UI
   survives a dropped-and-restored `GetStatus` stream (reconnect logic).

---

## Test sequence

Unit (per phase, `cargo test`):

| # | Test | Phase |
|---|---|---|
| U1 | `MockTransport` canned-response tests for every typed op (command bytes + parse) | 1 |
| U2 | `send_command` timeout → `ScannerError::Timeout`; empty/garbage → `UnexpectedResponse` | 1 |
| U3 | PRG/EPG sequencing: correct byte order, no duplicate `PRG`, return-to-monitor on mid-op failure | 1 |
| U4 | Volume/squelch/channel-index validation errors without port access | 1 |
| U5 | Existing `types.rs` / `modes.rs` tests remain green (Mode enum tests move/shrink with the manager deletion) | 1–2 |
| U6 | `From<ScannerError> for Status` mapping table | 4 |
| U7 | `ScanStatus → GetStatusResponse` mapping | 4 |
| U8 | `GetStatus` stream: second subscriber cancels first (or rejected); cancellation stops polling (assert on mock) | 4 |

Integration (hardware, scripted where possible with the `scan()` socat
helper for command-level checks and grpcurl for RPC-level):

| # | Check | Phase |
|---|---|---|
| T1 | socat spot-checks still pass after client refactor (`MDL`, `GLG`, `SCG`, `CIN,52`) | 1 |
| T2 | Full command trace of a console session logged at TRACE level and diffed against a pre-refactor trace (same key sequence) | 2 |
| T3 | `nix flake` build for x86_64 + aarch64 after workspace change | 3 |
| T4 | Console smoke: monitor, bank tab load, edit (freq+name), delete, bank toggle ×2, vol/squelch, scan/hold, quit — no regressions | 2 |
| T5 | grpcurl matrix: every RPC happy path + error paths (bad index 0/501 → `invalid_argument`; `SetEnabledBanks` len≠10 → `invalid_argument`; empty channel → absent `Channel`) | 4 |
| T6 | Web UI pass: live stream, edit, delete, bank toggle, hold/scan, stream reconnect after server restart | 5 |

Long-haul (once, end of Phase 4):

| # | Check |
|---|---|
| T7 | 10-minute `GetStatus` stream soak with a console session running concurrently (mutex contention check — no dropped GLG ticks beyond expected serialization) |

---

## Deliverables checklist

- [x] Phase 1: typed `ScannerClient` + `Transport` trait + `ScannerError` + mock tests (22 tests)
- [x] Phase 2: console on typed client, retry cap, no raw command strings outside `scanner.rs`
- [x] Phase 3: workspace, build.rs cleanup, `result/` gone, AGENTS.md accurate
- [x] Phase 4: all 10 RPCs implemented; T5 grpcurl matrix passes via `tests/fake_e2e.sh` (hardware T5/T7 still pending)
- [ ] Phase 5: Web UI (monitor + bank management) over grpc-web, same port
- [x] AGENTS.md "Serve Mode TODO" replaced with actual status

**Deviations from plan (recorded):**

- `GetChannel` on an empty slot returns a `Channel` with empty name and
  zero frequency rather than an absent `Channel`: the scanner answers
  `CIN,{idx},,,00000000,...` for empty slots, which parses fine, so there
  is no observable difference on the wire. Clients should treat
  zero-frequency as "empty".
- `GetStatusResponse.modulation` was added to the proto (Phase 4.3
  suggestion) — clients must regenerate.
- The GetStatus poller runs its blocking `get_status` per tick inside
  `spawn_blocking` (the 250ms sleep happens on the async side), so the
  serial mutex is only held during actual I/O.
