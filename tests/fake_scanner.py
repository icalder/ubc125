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
        elif name == "CIN" and len(cmd.split(",")) == 2:
            idx = cmd.split(",")[1]
            send(f"CIN,{idx},BHX RADAR,01239750,AM,0,0,0,0")
        elif name in ("PRG", "EPG", "KEY", "DCH", "VOL", "SQL", "SCG", "CIN"):
            send(cmd if name in ("VOL", "SQL", "SCG", "CIN") else name)
        else:
            send("NG")
