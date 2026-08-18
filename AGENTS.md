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

## Serve Mode

Exposes a gRPC (and gRPC-Web) interface to the scanner for remote control.  The gRPC service is defined in [services.proto](./lib/grpc/proto/ubc125/v1/services.proto).

Code for the serve mode is in [serve.rs](./src/cmd/serve.rs).  The gRPC handler methods are defined in [server.rs](./src/server.rs).  All RPCs are implemented.

Audio: `AudioService/Listen` streams the scanner's audio as WebM/Opus chunks (first chunk is the WebM header/init segment, then cluster-sized media chunks). The capture (ALSA mic via `ffmpeg`, or `UBC125_AUDIO_CMD` for tests) starts lazily on the first `Listen` and is keyed to that client's id; `AudioService/StopCapture` stops it for that id (a browser fetch abort does not close the TCP connection, so without it the capture would keep holding the audio device). See [AUDIO-PLAN.md](./AUDIO-PLAN.md) / [AUDIO-IMPL.md](./AUDIO-IMPL.md); Rust audio code in [src/audio/](./src/audio/mod.rs).

The server enables `accept_http1`, a permissive CORS layer, and per-service `GrpcWebLayer`s, so browsers can talk to it directly over grpc-web.  On the same port it also serves the web UI: everything that is not a gRPC path falls through to an axum static router over `web/dist` (embedded at compile time with `rust-embed`; see [web.rs](./src/web.rs)).  `GrpcWebLayer` is applied per service (not as a global fallback) because it 400s all non-grpc-web HTTP/1.1.

### Web UI

A vanilla-ESM browser console (no framework, no build step) in [web/](./web/README.md).  It mimics the terminal console: Monitor view (scanner info, live amber scan box from the `GetStatus` stream, tappable bank toggles, Scan/Hold, Play/Stop audio with a coloured state label) plus ten Bank views (50-row channel table, tap-to-select cursor, edit modal, delete confirm).  Audio plays through `lib/audio.js` (MediaSource/SourceBuffer, attached via `URL.createObjectURL` — Chromium rejects `srcObject` for main-thread `MediaSource`; bounded buffer, backoff reconnect).  Every action works by key *and* by pointer/touch; the layout is responsive from 390 px phones to desktop.  `web/dist` is committed and embedded — the Nix build stays node-free.  Browser E2E scripts live in [tests/web/](./tests/web_e2e.md).

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
   nix-shell -p socat ffmpeg --run 'node tests/web/web_audio_test.mjs'  # audio Play/Stop + throttle (18 checks, ~2 min; manages the stack itself)
   ```

   `web_hiccup_phone_test.mjs` kills and restarts the fake stack itself. W6 (`tests/web/web_hw_test.mjs`) runs the same list against `serve --device /dev/ttyACM0` with round-trip-only writes; current pass counts are recorded in [tests/web_e2e.md](./tests/web_e2e.md).

### Trying it with grpcurl

```sh
grpcurl -plaintext localhost:50051 ubc125.v1.SystemInfoService/GetModelInfo
grpcurl -plaintext localhost:50051 ubc125.v1.SystemInfoService/GetFirmwareVersion
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
- Audio: `src/audio/` — `ffmpeg.rs` spawns the capture (ALSA → Opus WebM to stdout; `UBC125_AUDIO_CMD` override), `webm.rs` splits the stream into cluster-sized segments, `broadcaster.rs` fans the segments out to `Listen` subscribers (per-subscriber bounded queue with send timeout; id-gated start/stop so one client's `StopCapture` cannot kill another's).

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