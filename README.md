# UBC 125 Serial Control

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