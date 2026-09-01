// Minimal smoke test: gRPC-Web calls against `ubc125 serve` (fake scanner).
// Run with: bash web/run_smoke.sh (starts the fake-scanner stack, then this).
// To point at a different server: node smoke.mjs http://other-host:50051
import { createClient } from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";
import * as pb from "./dist/proto/ubc125/v1/services_pb.js";

const baseUrl = process.argv[2] ?? "http://127.0.0.1:50051";
const transport = createGrpcWebTransport({ baseUrl });
const sys = createClient(pb.SystemInfoService, transport);
const scanner = createClient(pb.ScannerControlService, transport);

const results = [];
function check(name, ok, detail) {
  results.push(ok);
  console.log(`${ok ? "PASS" : "FAIL"} ${name}: ${detail}`);
}

const model = await sys.getModelInfo({});
check("model", model.result.includes("UBC125XLT"), model.result);

const fw = await sys.getFirmwareVersion({});
check("firmware", fw.result.length > 0, fw.result);

let status;
for await (const s of scanner.getStatus({})) {
  status = s;
  break;
}
check(
  "status stream",
  status.frequency.length > 0 && typeof status.signalDetected === "boolean",
  `freq=${status.frequency} signal=${status.signalDetected} mod=${status.modulation}`,
);

const banks = await scanner.getEnabledBanks({});
check("banks", banks.banks.length === 10, banks.banks.map((b) => (b ? 1 : 0)).join(""));

// Leaving the status stream early keeps its response socket open, so the event
// loop never drains by itself: exit with the verdict instead of hanging.
process.exit(results.every(Boolean) ? 0 : 1);
