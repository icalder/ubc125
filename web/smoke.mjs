// Minimal smoke test: gRPC-Web call against the serve binary (fake scanner).
// Run with: ./run_smoke.sh (starts socat pair + fake scanner + serve, then this).
import { createClient } from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";
import * as pb from "./dist/proto/ubc125/v1/services_pb.js";

const transport = createGrpcWebTransport({ baseUrl: "http://127.0.0.1:50198" });
const sys = createClient(pb.SystemInfoService, transport);
const model = await sys.getModelInfo({});
console.log("MODEL:", model.result);
const fw = await sys.getFirmwareVersion({});
console.log("FW:", fw.result);

const scanner = createClient(pb.ScannerControlService, transport);
for await (const s of scanner.getStatus({})) {
  console.log("STATUS freq:", s.frequency, "signal:", s.signal_detected, "mod:", s.modulation);
  break;
}
const banks = await scanner.getEnabledBanks({});
console.log("BANKS:", banks.banks.join(""));
