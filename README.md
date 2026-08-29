# UBC 125 Serial Control

## Quick build, deploy and test

```sh
# Build for x86
nix build .#ubc125

# Build the package for AArch64
nix build .#ubc125-aarch64
readlink -f ./result # to get store path

# Push the result to the Pi and run it
nix-copy-closure --to itcalde@alarmpi ./result
/nix/store/zhrs4vfqph0vikr4v93g2z3psy4xqp1j-ubc125-aarch64-unknown-linux-gnu-0.2.0/bin/ubc125 console
```

## NixOS service (Pi)

The flake ships a NixOS module (`nixosModules.default`) that runs `ubc125 serve` as a systemd service — no more copying the binary and babysitting it in `screen`:

```nix
# /etc/nixos/configuration.nix
imports = [
  (builtins.fetchTarball "https://github.com/<you>/ubc125/archive/<ref>.tar.gz")
    + "/flake.nix" # or however you reference the flake
];

services.ubc125 = {
  enable = true;
  # listenAddress = "0.0.0.0:50051";  # default
  # device = "/dev/ttyACM0";          # default: auto-detect by USB id
  # audioDevice = "hw:2";             # default: the Pi's USB mic
  # declick = true;                   # default: false, experimental audio de-clicker
};
```

Then `nixos-rebuild switch` (on the Pi, which builds the aarch64 package for you) and:

```sh
systemctl status ubc125-serve
journalctl -u ubc125-serve -f
```

Notes:

- The service runs as a dynamic user with the `dialout` (ttyACM* access) and `audio` (ALSA mic capture) groups.
- If the scanner is not connected at boot, `serve` fails and systemd retries every 10 s until it appears.
- The web UI and gRPC/gRPC-Web listen on the same port (`0.0.0.0:50051` by default), so it is reachable from other machines on the LAN.
- `declick = true` enables the de-click filter on the audio pipeline: a plateau-trigger de-clicker (`src/audio/clickfilter/`, a 1:1 port of `../ubc125-ml/src/clickfilter/`) running the T3 record config — interpolation for short clicks, descend for long ones, 150 ms long tail — with a fixed 20.5 ms output delay (the first chunk of every capture generation is silence). Same as the `serve --declick` flag / `UBC125_DECLICK` env var. It is off by default and only affects the native ALSA audio capture, not the `--audio-cmd` test hook.

For console mode (the Ratatui TUI) you still need a real TTY: `ssh -t itcalde@alarmpi .../ubc125 console` or a physical terminal.

## Minicom
nix-shell -p minicom
minicom --device /dev/ttyACM0
CtrlA-E # local echo
CtrlA-Q # quit

## UBC125 Commands
```text
VOL>
VOL,6>
Scan bank 2
PRG
SCG,1011111111
EPG

Undocumented scan status command!!!
https://github.com/pa3ang/ubc125xlt
GLG

Also another status command:
STS

From scan125 ilspy:
scan banks
KEY,S,P
hold key # can send again to toggle
KEY,H,P

PWR?

So to program then restart scan:
PRG
…
EPG
KEY,S,P

To hold scan on a channel:
KEY,H,P
and restart:
KEY,S,P

```