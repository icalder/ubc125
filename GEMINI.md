# UBC125XLT Radio Scanner Control

## Summary

This rust project will contain control programs for the UBC125XLT radio scanner.  The scanner can be programmed via a USB serial port.

### Console Mode

The aim here is to have a [Ratatui](https://ratatui.rs/) console interface mimicking the display and button panel of the actual scanner, with some extra screens for easy management of frequency banks.

Code for the console mode is in [cli.rs](./src/cmd/cli.rs).

### Serve Mode

TODO, this will expose a gRPC interface to the scanner for remote control.

Code for the serve mode is in [serve.rs](./src/cmd/serve.rs).

## Documentation

[Scanner Commands](./SCANNER-COMMANDS.md) is a reference for all the serial commands supported by the scanner.  Some of these are documented by Uniden and some have been discovered by reverse-engineering.  The document also includes some examples of command usage.