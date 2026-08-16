#!/usr/bin/env bash
# Start the fake-scanner stack for web E2E (W5): socat pty pair ->
# `python3 tests/fake_scanner.py` -> `ubc125 serve` on 127.0.0.1:50051.
# Idempotent: kills any old stack first. Safe to run from execSync: all
# background processes redirect their stdio so they never hold the caller's
# pipes open (which would block a piped shell until timeout).
set -u
# socat is not on the default PATH; re-exec under a nix env that has it.
if ! command -v socat >/dev/null 2>&1; then
  exec nix-shell -p socat --run "bash $0"
fi
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
A=/tmp/w5A B=/tmp/w5B PORT=50051

pgrep -x ubc125 | xargs -r kill 2>/dev/null
pgrep -x socat | xargs -r kill 2>/dev/null
pgrep -f 'fake_sc[a]nner' | xargs -r kill 2>/dev/null
sleep 0.5
rm -f $A $B

socat pty,raw,echo=0,link=$A pty,raw,echo=0,link=$B >/dev/null 2>&1 &
for i in $(seq 1 50); do [ -L $A ] && [ -L $B ] && break; sleep 0.1; done
python3 $ROOT/tests/fake_scanner.py $A >>/tmp/fake.log 2>&1 &
$ROOT/target/debug/ubc125 serve --device $B --server-addr 127.0.0.1:$PORT >>/tmp/serve.log 2>&1 &
sleep 1
