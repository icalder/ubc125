#!/usr/bin/env bash
# End-to-end test of the gRPC server against a fake UBC125XLT scanner.
#
# The fake scanner (tests/fake_scanner.py) speaks the serial protocol on one
# end of a socat pty pair; `ubc125 serve` connects to the other end. grpcurl
# then exercises every RPC, including the error paths.
#
# Requires: socat, grpcurl, python3, curl
#   nix-shell -p socat grpcurl curl --run 'bash tests/fake_e2e.sh'
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BIN=$ROOT/target/debug/ubc125
A=/tmp/fakeA B=/tmp/fakeB PORT=50099

cleanup() {
    kill $FAKE_PID $UBC_PID $SOCAT_PID 2>/dev/null
    rm -f $A $B
}
trap cleanup EXIT

socat pty,raw,echo=0,link=$A pty,raw,echo=0,link=$B &
SOCAT_PID=$!
for i in $(seq 1 50); do [ -L $A ] && [ -L $B ] && break; sleep 0.1; done

python3 $ROOT/tests/fake_scanner.py $A &
FAKE_PID=$!

$BIN serve --device $B --server-addr 127.0.0.1:$PORT &
UBC_PID=$!
sleep 1

g() { # g [-d JSON] rpc... — inserts the host before the positional args
    local data=""
    if [ "$1" = "-d" ]; then data="$1 $2"; shift 2; fi
    grpcurl -plaintext $data 127.0.0.1:$PORT "$@" 2>&1
}
pass=0; fail=0
check() { # name, expected-substring, actual
    if echo "$3" | grep -q "$2"; then echo "PASS: $1"; pass=$((pass+1));
    else echo "FAIL: $1"; echo "  expected substring: $2"; echo "  got: $3"; fail=$((fail+1)); fi
}
check_ok() { # name, actual — success means no ERROR from grpcurl
    if echo "$2" | grep -q "ERROR"; then echo "FAIL: $1"; echo "  got: $2"; fail=$((fail+1));
    else echo "PASS: $1"; pass=$((pass+1)); fi
}

SVC=ubc125.v1.ScannerControlService
SYS=ubc125.v1.SystemInfoService

out=$(g $SYS/GetModelInfo);            check "GetModelInfo"        "MDL,UBC125XLT" "$out"
out=$(g $SYS/GetFirmwareVersion);      check "GetFirmwareVersion"  "Version 1.00.00" "$out"
out=$(g $SVC/GetAudioSettings);        check "GetAudioSettings"    "\"volume\"" "$out"
out=$(g $SVC/GetEnabledBanks);         check "GetEnabledBanks"     "false" "$out"

out=$(g -d '{"banks":[true,false]}' $SVC/SetEnabledBanks)
check "SetEnabledBanks bad len" "InvalidArgument" "$out"
out=$(g -d '{"banks":[true,true,true,true,true,true,true,true,true,true]}' $SVC/SetEnabledBanks)
check_ok "SetEnabledBanks ok" "$out"

out=$(g -d '{"index":52}' $SVC/GetChannel);  check "GetChannel 52"  "BHX RADAR" "$out"
out=$(g -d '{"index":0}' $SVC/GetChannel);   check "GetChannel 0"   "InvalidArgument" "$out"
out=$(g -d '{"index":501}' $SVC/GetChannel); check "GetChannel 501" "InvalidArgument" "$out"

out=$(g -d '{"channel":{"index":52,"name":"TEST","frequency":"121.5","modulation":"FM"}}' $SVC/SetChannel)
check_ok "SetChannel ok" "$out"
out=$(g -d '{"channel":{"index":52,"name":"X","frequency":"nope"}}' $SVC/SetChannel)
check "SetChannel bad freq" "InvalidArgument" "$out"
out=$(g -d '{}' $SVC/SetChannel)
check "SetChannel missing" "InvalidArgument" "$out"

out=$(g -d '{"index":52}' $SVC/DeleteChannel);  check_ok "DeleteChannel ok" "$out"
out=$(g -d '{"index":999}' $SVC/DeleteChannel); check "DeleteChannel 999" "InvalidArgument" "$out"

out=$(g -d '{}' $SVC/StartScan); check_ok "StartScan" "$out"
out=$(g -d '{}' $SVC/HoldScan);  check_ok "HoldScan" "$out"

# The fake answers every CIN read with a populated channel, so a full
# bank lists all 50 slots.
out=$(g -d '{"bank":1}' $SVC/ListChannels)
check "ListChannels bank 1" "BHX RADAR" "$out"
n=$(echo "$out" | grep -c '"index"'); [ "$n" = "50" ] && { echo "PASS: ListChannels 50 channels"; pass=$((pass+1)); } || { echo "FAIL: ListChannels 50 channels (got $n)"; fail=$((fail+1)); }
out=$(g -d '{"bank":2}' $SVC/ListChannels);  check "ListChannels bank 2 first index" "\"index\": 51" "$out"
out=$(g -d '{"bank":0}' $SVC/ListChannels);  check "ListChannels bank 0"  "InvalidArgument" "$out"
out=$(g -d '{"bank":11}' $SVC/ListChannels); check "ListChannels bank 11" "InvalidArgument" "$out"

# Static file serving: / returns the embedded index.html; gRPC paths
# are unaffected (the grpcurl calls above already prove that).
out=$(curl -s http://127.0.0.1:$PORT/)
check "static / serves index.html" "<html" "$out"
out=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/nope.js)
check "static missing file 404" "404" "$out"

out=$(timeout 2 grpcurl -plaintext -d '{}' 127.0.0.1:$PORT $SVC/GetStatus 2>&1 | head -20)
check "GetStatus stream"     "123.9750" "$out"
check "GetStatus modulation" "AM" "$out"

out=$(g list $SVC)
for m in GetAudioSettings StartScan HoldScan GetEnabledBanks SetEnabledBanks GetStatus GetChannel SetChannel DeleteChannel ListChannels; do
    echo "$out" | grep -q "$m" || { echo "FAIL: reflect missing $m"; fail=$((fail+1)); }
done
out=$(g list $SYS)
for m in GetModelInfo GetFirmwareVersion; do
    echo "$out" | grep -q "$m" || { echo "FAIL: reflect missing $m"; fail=$((fail+1)); }
done
echo "PASS: reflection lists all methods"

echo "=== $pass passed, $fail failed ==="
exit $fail
