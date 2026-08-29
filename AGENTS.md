# UBC125XLT Radio Scanner Control

## Summary

This rust project will contain control programs for the UBC125XLT radio scanner.  The scanner can be programmed via a USB serial port.

## Key Scanner Concepts

The scanner can store up to 500 frequencies.  They are divided up into 10 channel storage banks with up to 50 channels in each bank.

Banks can be enabled for scanning.  The scanner scans through all unlocked channels in the enabled banks in channel order.  When the scanner finds a transmission it stops on it.

## Key Functional Requirements

The scanner has lots of features but we are only interested in the core scanning functionality.  The features required are:

 - Select which channel banks are being scanned
 - Get a real-time view of scanning activity
 - When the scanner stops on a transmission, to see the channel that has been hit
 - List the channels in a bank
 - Edit the channels in a bank

## Console Mode

The aim here is to have a [Ratatui](https://ratatui.rs/) console interface mimicking the display and button panel of the actual scanner, with some extra screens for easy management of frequency banks.

Code for the console mode is in [console.rs](./src/cmd/console.rs).

Scanner port: `--device` (serve) and `--console-device` (console) are optional. When omitted, the port is auto-detected from the scanner's built-in USB id (1965:0018, Uniden UBC125XLT) among `ttyACM*`/`ttyUSB*` devices — ACM before USB, numeric order — by reading sysfs ([detect.rs](./src/detect.rs)); nothing is opened or probed. `UBC125_DEVICE` sets the port for both modes.

## Serve Mode

Exposes a gRPC (and gRPC-Web) interface to the scanner for remote control.  The gRPC service is defined in [services.proto](./lib/grpc/proto/ubc125/v1/services.proto).

Code for the serve mode is in [serve.rs](./src/cmd/serve.rs).  The gRPC handler methods are defined in [server.rs](./src/server.rs).  All RPCs are implemented.

Audio: `AudioService/Listen` streams the scanner's audio as WebM/Opus chunks (first chunk is the WebM header/init segment, then cluster-sized media chunks). The capture (ALSA mic via the native `alsa`+`opus` pipeline, or `UBC125_AUDIO_CMD` for tests; the hidden `ubc125 audio-tone` subcommand is the ffmpeg-free test fixture) starts lazily on the first `Listen` and is keyed to that client's id; `AudioService/StopCapture` stops it for that id (a browser fetch abort does not close the TCP connection, so without it the capture would keep holding the audio device). Rust audio code in [src/audio/](./src/audio/mod.rs).

De-clicker: `--declick` (or `UBC125_DECLICK`) enables the de-click filter on the ALSA capture only: [src/audio/clickfilter/](./src/audio/clickfilter/mod.rs) is a 1:1 port of `../ubc125-ml/src/clickfilter/` (plateau-trigger de-clicker; `SquelchGate` in `src/audio/squelch.rs` is removed) running the T3 record config from [../ubc125-ml/docs/prototype.md](../ubc125-ml/docs/prototype.md) (interp short / descend long, 150 ms long tail), wired in [serve.rs](./src/cmd/serve.rs). Fixed 20.5 ms output delay — the first chunk of each capture generation is silence. `--audio-cmd` and `audio-tone` stay unfiltered. Offline harness: `cargo run --example declick <wav>`; seam tests in [tests/clickfilter_seam.rs](./tests/clickfilter_seam.rs), sample-for-sample parity with the Python reference in [tests/clickfilter_parity.rs](./tests/clickfilter_parity.rs).

De-clicker tuning target: raw ALSA PCM captured on the Pi over an SSH pipe (`ssh alarmpi 'arecord -D hw:2 -f S16_LE -r 48000 -c 1 -t raw -d 60' > raw.s16`, wrapped to WAV in [test-audio/](./test-audio/README.md)) — **not** the Opus-decoded `Listen` stream: the `clickfilter` runs before Opus encoding, so tuning must validate against the signal it actually sees. Run it with `cargo run --example declick <wav>`, and see [../ubc125-ml/docs/prototype.md](../ubc125-ml/docs/prototype.md) for the T3 config of record.

The server enables `accept_http1`, a permissive CORS layer, and per-service `GrpcWebLayer`s, so browsers can talk to it directly over grpc-web.  On the same port it also serves the web UI: everything that is not a gRPC path falls through to an axum static router over `web/dist` (embedded at compile time with `rust-embed`; see [web.rs](./src/web.rs)).  `GrpcWebLayer` is applied per service (not as a global fallback) because it 400s all non-grpc-web HTTP/1.1.

### Web UI

A vanilla-ESM browser console (no framework, no build step) in [web/](./web/README.md).  It mimics the terminal console: Monitor view (scanner info, live amber scan box from the `GetStatus` stream, tappable bank toggles, Scan/Hold, Play/Stop audio with a coloured state label) plus ten Bank views.  The `GetStatus` stream also carries the bank mask (`GetStatusResponse.banks`), so a bank toggle in one tab/browser reaches every other connected tab within one poll (250 ms); the client refreshes `state.banks` from each stream message (50-row channel table, tap-to-select cursor, edit modal, delete confirm).  Audio plays through `lib/audio.js` (MediaSource/SourceBuffer, attached via `URL.createObjectURL` — Chromium rejects `srcObject` for main-thread `MediaSource`; bounded buffer, backoff reconnect).  Every action works by key *and* by pointer/touch; the layout is responsive from 390 px phones to desktop.  `web/dist` is committed and embedded — the Nix build stays node-free.  Browser E2E scripts live in [tests/web/](./tests/web_e2e.md).

### Testing the Web UI

Two layers (details in [web/README.md](./web/README.md) and [tests/web_e2e.md](./tests/web_e2e.md)):

1. **Unit tests** — pure client logic (`freq.js`, bank labels/ranges/cursor math, backoff schedule), no browser or server needed:

   ```sh
   cd web && node --test dist/tests/*.test.js
   ```

2. **Browser E2E** — scripted CDP sessions against a *fake scanner* (W5; `tests/fake_scanner.py` on a socat pty pair) or the real hardware (W6). The fake stack is started with `bash tests/ubc125_stack.sh` (idempotent; self-provisions `socat` via `nix-shell`; serves on `127.0.0.1:50051`). The browser must be Edge launched by the **browser-tools skill** (`browser-start.js`, CDP on `:9222`) — the E2E scripts connect to it with `puppeteer-core` imported from that skill's `node_modules`; nothing in the repo installs a browser. Then:

   ```sh
   bash tests/ubc125_stack.sh                 # fake scanner + serve (W5)
   node tests/web/web_pointer_test.mjs        # 1280x720 pointer path (26 checks)
   node tests/web/web_hiccup_phone_test.mjs   # offline banner + 390x844 phone (10 checks)
   node tests/web/web_two_tabs_test.mjs       # KI-2: two tabs both stay ONLINE (8 checks, ~25 s)
   node tests/web/web_bank_sync_test.mjs      # bank-sync: tab 1's bank toggle must reach tab 2 (11 checks, ~20 s)
   node tests/web/web_audio_test.mjs   # audio Play/Stop + throttle + late joiner (22 checks, ~3 min; needs target/debug/ubc125, manages the stack itself)
   ```

   `web_hiccup_phone_test.mjs` kills and restarts the fake stack itself. W6 (`tests/web/web_hw_test.mjs`) runs the same list against `serve --device /dev/ttyACM0` with round-trip-only writes; current pass counts are recorded in [tests/web_e2e.md](./tests/web_e2e.md).

### Trying it with grpcurl

```sh
grpcurl -plaintext localhost:50051 ubc125.v1.SystemInfoService/GetModelInfo
grpcurl -plaintext localhost:50051 ubc125.v1.SystemInfoService/GetFirmwareVersion
```

### Audio diagnostics (CLI)

[examples/audio_dump.rs](./examples/audio_dump.rs) is a tonic gRPC client for `AudioService/Listen` — the same bytes the server sends, without a browser. It is the bisection tool for audio problems: if the CLI is clean but a browser duplicates/stalls, the fault is client-side (MSE code or the grpc-web layer); if the CLI also misbehaves, the server bytes carry it.

Positional args: `[addr] [prefix] [seconds] [streams] [join-delay-secs] [stopgap-secs] [play]` (defaults: `http://192.168.1.90:50051`, `/tmp/ubc125-dump`, `20`, `1`, `3`, `0`, off).

- **Dump** (default): captures N streams (each after its join delay) for {seconds} s, writes `<prefix>_<n>.webm` + an in-process Opus-decoded `<prefix>_<n>.wav` (48 kHz mono), and reports per-cluster timecode continuity (byte-duplicates, overlaps, gaps) per stream plus a cross-stream report of whether a late stream joined live or replayed from 0.

  ```sh
  cargo run --example audio_dump http://192.168.1.90:50051 /tmp/dump 20 2 3   # two streams, second joins 3 s late
  ```

- **Play**: streams the decoded audio as a size-unknown WAV on stdout (progress on stderr) for by-ear testing through a local player — the CLI equivalent of the browser's Play button:

  ```sh
  cargo run --example audio_dump http://192.168.1.90:50051 /tmp/x 30 1 0 0 play | paplay
  ```

- **Stopgap** (6th arg > 0, single stream): plays {seconds} s, sends StopCapture, waits {stopgap} s (as audible silence in play mode), then plays a second generation for {seconds} s — reproduces the stop→play boundary:

  ```sh
  cargo run --example audio_dump http://192.168.1.90:50051 /tmp/x 12 1 0 2 play | paplay
  ```

## gRPC code generation

Generated prost/tonic code is committed in [lib/grpc/rust-gen/src/proto](./lib/grpc/rust-gen/src/proto) so the package builds without a protobuf toolchain.  `build.rs` only produces the file descriptor set (for reflection).  After changing the `.proto` files, regenerate and commit:

```sh
UBC125_REGEN=1 cargo build -p ubc125-grpc
```

## Architecture

`ScannerClient` in [scanner.rs](./src/scanner.rs) is the single command layer for the scanner, used by both the console and the gRPC server.  It exposes typed operations (`get_status`, `get_banks`, `set_banks`, `get_channel`, `get_bank_channels`, `set_channel`, `delete_channel`, `start_scan`, `hold_scan`, ...) that validate responses and manage the scanner's program mode (PRG/EPG) internally.  `send_command` remains as a raw escape hatch.

- Byte-level I/O goes through the `Transport` trait (`SerialTransport` in production); `ScannerClient::with_transport` accepts a mock for tests.
- Communication/validation failures surface as `ScannerError`; the gRPC server maps it to status codes (`invalid_argument` / `unavailable` / `internal`).
- Scanner response parsing (GLG, CIN, SCG) and domain types (`Frequency`, `Channel`, `ChannelIndex`, `BankMask`, `ScanStatus`) live in [types.rs](./src/types.rs).
- The gRPC server shares one client behind `Arc<Mutex<ScannerClient>>`; blocking serial I/O runs in `spawn_blocking`.
- Status: `src/status.rs` — `StatusBroadcaster` runs one shared `GLG` poll task for any number of `GetStatus` subscribers (first starts it, last stops it, KI-2). Each broadcast is a `StatusUpdate { status, banks }`: the bank mask is cached server-side, fast-forwarded by `SetEnabledBanks`/`GetEnabledBanks`, and re-read from the radio every 120th poll (~30 s) so bank buttons pressed on the unit itself also reach the clients.
- Audio: `src/audio/` — `native.rs` runs the capture natively (ALSA `PcmReader` → `FrameSplitter` → `OpusFrameEncoder` → `WebmMuxer`, no child process; `AlsaOpusSource` for the device, `ToneSource`/`audio-tone` for tests), `source.rs` holds the `CaptureSource` trait + `CommandSource` (`UBC125_AUDIO_CMD` override), `webm.rs` splits the stream into cluster-sized segments, `broadcaster.rs` fans the segments out to `Listen` subscribers (per-subscriber bounded queue with send timeout; id-gated start/stop so one client's `StopCapture` cannot kill another's).

## Documentation

[Scanner Commands](./SCANNER-COMMANDS.md) is a reference for all the serial commands supported by the scanner.  Some of these are documented by Uniden and some have been discovered by reverse-engineering.  The document also includes some examples of command usage.

## Testing Scanner commands

`socat` is available (wrap with `nix-shell -p socat --run` if it is not on the PATH) and can be used like in the examples below:

```sh
echo -ne "MDL\r" | socat -t 1 - /dev/ttyACM0,b115200,raw,echo=0 | tr '\r' '\n'
echo -ne "GLG\r" | socat -t 1 - /dev/ttyACM0,b115200,raw,echo=0 | tr '\r' '\n'
```

to make testing easier a helper shell function can be created:

```sh
scan() {
    echo -ne "$1\r" | socat -t 0.5 - /dev/ttyACM0,b115200,raw,echo=0 | tr '\r' '\n'
}

# usage: scan MDL
```