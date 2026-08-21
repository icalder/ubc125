#!/usr/bin/env python3
"""Stream a WebM file to stdout paced over a fixed duration.

For the audio E2E: `cat` would dump the whole
file in milliseconds, the server's broadcast channel (64 slots) would
overflow before the browser's first poll, and the stream would die with a
Lagged error before a single chunk was delivered. Pacing the file over a
few seconds keeps the faster-than-real-time stream slow enough for the
client to keep up, while still ending well inside the test's wait windows.

Usage: paced_file.py FILE SECONDS
"""

import sys
import time


def main() -> None:
    path = sys.argv[1]
    seconds = float(sys.argv[2])
    data = open(path, "rb").read()
    steps = 20
    chunk = max(len(data) // steps, 1)
    per_step = seconds / steps
    out = sys.stdout.buffer
    for i in range(0, len(data), chunk):
        out.write(data[i : i + chunk])
        out.flush()
        time.sleep(per_step)


if __name__ == "__main__":
    main()
