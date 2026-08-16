#!/usr/bin/env python3
"""Fake UBC125XLT scanner: answers serial commands on a pty like the real radio."""
import os
import sys
import termios
import tty

path = sys.argv[1]
fd = os.open(path, os.O_RDWR | os.O_NOCTTY)
attrs = tty.tcgetattr(fd)
tty.setraw(fd)
attrs = tty.tcgetattr(fd)
attrs[3] &= ~termios.ECHO
termios.tcsetattr(fd, termios.TCSANOW, attrs)


def send(s: str) -> None:
    os.write(fd, (s + "\r").encode())


# Channel storage: idx -> (name, freq, mod). Reads of unwritten channels
# return a default "pre-programmed" value, so the radio behaves like a
# scanner with all 500 channels programmed. Deleted channels read back as
# empty (zero frequency), matching how get_bank_channels detects holes.
channels = {}
deleted = set()


def channel(idx: str):
    return channels.setdefault(idx, ("BHX RADAR", "01239750", "AM"))


buf = b""
while True:
    try:
        data = os.read(fd, 256)
    except OSError:
        break
    if not data:
        break
    buf += data
    while b"\r" in buf:
        line, buf = buf.split(b"\r", 1)
        cmd = line.decode(errors="replace").strip()
        if not cmd:
            continue
        name = cmd.split(",")[0]
        if name == "MDL":
            send("MDL,UBC125XLT")
        elif name == "VER":
            send("VER,Version 1.00.00")
        elif name == "GLG":
            send("GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52,")
        elif name == "SCG" and cmd == "SCG":
            send("SCG,0101010101")
        elif name == "CIN":
            parts = cmd.split(",")
            if len(parts) == 2:  # read
                idx = parts[1]
                if idx in deleted:
                    send(f"CIN,{idx},,00000000,FM,0,0,0,0")
                else:
                    n, f, m = channel(idx)
                    send(f"CIN,{idx},{n},{f},{m},0,0,0,0")
            else:  # write: CIN,idx,name,freq,mod,...
                channels[parts[1]] = (parts[2], parts[3], parts[4])
                deleted.discard(parts[1])
                send(cmd)
        elif name == "DCH":
            idx = cmd.split(",")[1]
            channels.pop(idx, None)
            deleted.add(idx)
            send(cmd)
        elif name == "VOL":
            send("VOL,15" if "," not in cmd else cmd)
        elif name == "SQL":
            send("SQL,05" if "," not in cmd else cmd)
        elif name in ("PRG", "EPG", "KEY", "SCG"):
            send(cmd if name == "SCG" else name)
        else:
            send("NG")
