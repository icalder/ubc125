# UBC 125 Serial Control

## Quick build, deploy and test

```sh
# Build the package for AArch64
nix build .#packages.aarch64-linux.ubc125
ssh # to get store path

# Push the result to the Pi and run it
nix-copy-closure --to itcalde@alarmpi ./result
/nix/store/2x8lqzkr2m8srwwcifak3yv0pjcs3ad0-ubc125-0.1.0/bin/ubc125 console
```

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