#!/usr/bin/env bash
# T5 (hardware) — non-destructive grpcurl matrix against the real scanner.
#
# Writes are round-trips only: banks and channel 52 are written back with
# the exact values that were read. No deletes.
#
# Requires: grpcurl. Device defaults to /dev/ttyACM0; override with DEVICE=.
#   nix-shell -p grpcurl --run 'bash tests/hw_e2e.sh'
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd $ROOT
PORT=50098
./target/debug/ubc125 serve --device ${DEVICE:-/dev/ttyACM0} --server-addr 127.0.0.1:$PORT &
UBC_PID=$!
trap 'kill $UBC_PID 2>/dev/null' EXIT
sleep 1

g() {
    local data=""
    if [ "$1" = "-d" ]; then data="$1 $2"; shift 2; fi
    grpcurl -plaintext $data 127.0.0.1:$PORT "$@" 2>&1
}
SVC=ubc125.v1.ScannerControlService
SYS=ubc125.v1.SystemInfoService
pass=0; fail=0
ok()   { echo "PASS: $1"; pass=$((pass+1)); }
bad()  { echo "FAIL: $1"; echo "  got: $2"; fail=$((fail+1)); }
assert_grep() { if echo "$2" | grep -q "$3"; then ok "$1"; else bad "$1" "$2"; fi; }
assert_ok()   { if echo "$2" | grep -q "ERROR"; then bad "$1" "$2"; else ok "$1"; fi; }

echo "== reads =="
out=$(g $SYS/GetModelInfo);            assert_grep "GetModelInfo"      "$out" "UBC125XLT"
out=$(g $SYS/GetFirmwareVersion);      assert_ok  "GetFirmwareVersion" "$out"
out=$(g $SVC/GetAudioSettings);        assert_grep "GetAudioSettings"  "$out" '"volume"'
out=$(g $SVC/GetEnabledBanks);         assert_ok  "GetEnabledBanks"    "$out"
BANKS_JSON=$(echo "$out" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["banks"]))')
echo "  current banks: $BANKS_JSON"

echo "== validation paths (no hardware access) =="
out=$(g -d '{"index":0}' $SVC/GetChannel);        assert_grep "GetChannel 0"      "$out" "InvalidArgument"
out=$(g -d '{"index":501}' $SVC/GetChannel);      assert_grep "GetChannel 501"    "$out" "InvalidArgument"
out=$(g -d '{"banks":[true,false]}' $SVC/SetEnabledBanks); assert_grep "SetEnabledBanks bad len" "$out" "InvalidArgument"
out=$(g -d '{"channel":{"index":52,"name":"X","frequency":"nope"}}' $SVC/SetChannel); assert_grep "SetChannel bad freq" "$out" "InvalidArgument"
out=$(g -d '{}' $SVC/SetChannel);                 assert_grep "SetChannel missing" "$out" "InvalidArgument"
out=$(g -d '{"index":0}' $SVC/DeleteChannel);     assert_grep "DeleteChannel 0"   "$out" "InvalidArgument"
out=$(g -d '{"index":999}' $SVC/DeleteChannel);   assert_grep "DeleteChannel 999" "$out" "InvalidArgument"

echo "== banks round-trip (write back exactly what was read) =="
out=$(g -d "{\"banks\":$BANKS_JSON}" $SVC/SetEnabledBanks); assert_ok "SetEnabledBanks (same values)" "$out"
out2=$(g $SVC/GetEnabledBanks)
BANKS_JSON2=$(echo "$out2" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["banks"]))')
[ "$BANKS_JSON" = "$BANKS_JSON2" ] && ok "banks unchanged" || bad "banks unchanged" "before=$BANKS_JSON after=$BANKS_JSON2"

echo "== channel 52 round-trip (write back exactly what was read) =="
out=$(g -d '{"index":52}' $SVC/GetChannel)
assert_ok "GetChannel 52" "$out"
CH_JSON=$(echo "$out" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["channel"]))')
echo "  channel 52: $CH_JSON"
out=$(g -d "{\"channel\":$CH_JSON}" $SVC/SetChannel); assert_ok "SetChannel 52 (same values)" "$out"
out=$(g -d '{"index":52}' $SVC/GetChannel)
CH_JSON2=$(echo "$out" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["channel"]))')
[ "$CH_JSON" = "$CH_JSON2" ] && ok "channel 52 unchanged" || bad "channel 52 unchanged" "before=$CH_JSON after=$CH_JSON2"

echo "== scan control =="
out=$(g -d '{}' $SVC/HoldScan);  assert_ok "HoldScan"  "$out"
out=$(g -d '{}' $SVC/StartScan); assert_ok "StartScan" "$out"

echo "== GetStatus stream =="
out=$(timeout 2 grpcurl -plaintext -d '{}' 127.0.0.1:$PORT $SVC/GetStatus 2>&1 | head -30)
assert_grep "GetStatus stream emits" "$out" '"frequency"'

echo "== reflection =="
out=$(g list $SVC); missing=""
for m in GetAudioSettings StartScan HoldScan GetEnabledBanks SetEnabledBanks GetStatus GetChannel SetChannel DeleteChannel; do
    echo "$out" | grep -q "$m" || missing="$missing $m"
done
[ -z "$missing" ] && ok "reflection lists all ScannerControlService methods" || bad "reflection" "missing:$missing"

echo "=== T5 hardware: $pass passed, $fail failed ==="
exit $fail
