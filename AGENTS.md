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

Code for the serve mode is in [serve.rs](./src/cmd/serve.rs).  The gRPC handler methods are defined in [server.rs](./src/server.rs).  `SystemInfoService` and `GetAudioSettings` are implemented; the remaining `ScannerControlService` RPCs are stubbed pending full implementation (see [PLAN.md](./PLAN.md)).

The server enables `accept_http1`, a permissive CORS layer, and `GrpcWebLayer`, so browsers can talk to it directly over grpc-web.

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

`ScannerClient` in [scanner.rs](./src/scanner.rs) is the single command layer for the scanner, used by both the console and the gRPC server.  It exposes typed operations (`get_status`, `get_banks`, `set_banks`, `get_channel`, `set_channel`, `delete_channel`, `start_scan`, `hold_scan`, ...) that validate responses and manage the scanner's program mode (PRG/EPG) internally.  `send_command` remains as a raw escape hatch.

- Byte-level I/O goes through the `Transport` trait (`SerialTransport` in production); `ScannerClient::with_transport` accepts a mock for tests.
- Communication/validation failures surface as `ScannerError`; the gRPC server maps it to status codes (`invalid_argument` / `unavailable` / `internal`).
- Scanner response parsing (GLG, CIN, SCG) and domain types (`Frequency`, `Channel`, `ChannelIndex`, `BankMask`, `ScanStatus`) live in [types.rs](./src/types.rs).
- The gRPC server shares one client behind `Arc<Mutex<ScannerClient>>`; blocking serial I/O runs in `spawn_blocking`.

## Documentation

[Scanner Commands](./SCANNER-COMMANDS.md) is a reference for all the serial commands supported by the scanner.  Some of these are documented by Uniden and some have been discovered by reverse-engineering.  The document also includes some examples of command usage.

## Testing Scanner commands

`socat` is available and can be used like in the examples below:

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