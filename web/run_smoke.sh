#!/usr/bin/env bash
# Run the minimal gRPC-Web smoke test (web/smoke.mjs) against the fake
# scanner: socat pty pair -> tests/fake_scanner.py -> `ubc125 serve` on
# 127.0.0.1:50051 (the stack from tests/ubc125_stack.sh).
#
#   bash web/run_smoke.sh          # start stack, run smoke, stop stack
#   bash web/run_smoke.sh --keep   # leave the stack up for further poking
#
# Safe to run from any directory. Logs: /tmp/fake.log, /tmp/serve.log.
set -uo pipefail

WEB="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$WEB/.." && pwd)"
PORT=50051

keep=0
[ "${1:-}" = "--keep" ] && keep=1

teardown() {
  [ "$keep" = 1 ] && return
  pgrep -x ubc125 | xargs -r kill 2>/dev/null
  pgrep -x socat | xargs -r kill 2>/dev/null
  pgrep -f 'fake_sc[a]nner' | xargs -r kill 2>/dev/null
}

if [ ! -x "$ROOT/target/debug/ubc125" ]; then
  echo "building debug binary..."
  (cd "$ROOT" && cargo build) || { echo "cargo build failed"; exit 1; }
fi

bash "$ROOT/tests/ubc125_stack.sh"

# The stack script sleeps 1 s; poll the port instead of trusting that.
ready=0
for _ in $(seq 1 50); do
  if timeout 1 bash -c "exec 3<>/dev/tcp/127.0.0.1/$PORT" 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.2
done
if [ "$ready" != 1 ]; then
  echo "serve did not open 127.0.0.1:$PORT — see /tmp/serve.log"
  tail -20 /tmp/serve.log
  teardown
  exit 1
fi

node "$WEB/smoke.mjs" "http://127.0.0.1:$PORT"
status=$?
teardown
exit $status
