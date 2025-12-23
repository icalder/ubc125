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

Code for the console mode is in [cli.rs](./src/cmd/cli.rs).

## Serve Mode

TODO, this will expose a gRPC interface to the scanner for remote control.

Code for the serve mode is in [serve.rs](./src/cmd/serve.rs).

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