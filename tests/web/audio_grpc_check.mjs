// Audio gRPC streaming check (AUDIO-IMPL 8.3).
//
// A real binary client for AudioService/Listen (grpcurl's text output cannot
// verify WebM payloads): connects over grpc-web to a running `ubc125 serve`
// with a D1 audio source, asserts init-then-media ordering and
// non-decreasing timestamps, and writes init+chunks to a file so ffprobe can
// validate the concatenation as WebM/Opus.
//
// Usage:
//   node tests/web/audio_grpc_check.mjs [baseUrl] [outFile] [seconds]
// Defaults: http://127.0.0.1:50051  /tmp/audio_grpc_check.webm  5
//
// Validate the written stream (needs ffmpeg tools):
//   nix-shell -p ffmpeg --run 'ffprobe -v error -show_entries stream=codec_name -of csv /tmp/audio_grpc_check.webm'
// Expected: a single "opus" stream.

import { writeFileSync } from "node:fs";
// connect-web is imported by explicit path: this script runs from tests/web/
// but the browser deps live in web/node_modules (their own bare imports
// resolve from there).
import { createClient } from "../../web/node_modules/@connectrpc/connect/dist/esm/index.js";
import { createGrpcWebTransport } from "../../web/node_modules/@connectrpc/connect-web/dist/esm/index.js";
import { AudioService } from "../../web/dist/proto/ubc125/v1/services_pb.js";

const baseUrl = process.argv[2] ?? "http://127.0.0.1:50051";
const outFile = process.argv[3] ?? "/tmp/audio_grpc_check.webm";
const seconds = Number(process.argv[4] ?? 5);

function fail(msg) {
  console.error(`FAIL: ${msg}`);
  process.exit(1);
}

const client = createClient(
  AudioService,
  createGrpcWebTransport({ baseUrl }),
);

const parts = [];
let gotInit = false;
let lastTs = Number.NEGATIVE_INFINITY;
let chunks = 0;
const deadline = Date.now() + seconds * 1000;

// A natural stream end (e.g. the `cat /tmp/cap.webm` D1 source finishing)
// is fine; we keep what we got and validate it below.
for await (const chunk of client.listen({})) {
  if (chunk.initSegment) {
    if (gotInit) fail("second init segment in the same stream");
    gotInit = true;
  } else if (!gotInit) {
    fail("media chunk before the init segment");
  }
  if (chunk.timestampMs < lastTs) {
    fail(`timestamp went backwards: ${lastTs} -> ${chunk.timestampMs}`);
  }
  lastTs = chunk.timestampMs;
  parts.push(chunk.payload);
  chunks++;
  if (Date.now() > deadline) break;
}

if (!gotInit) fail("no init segment received");
if (chunks < 2) fail(`expected init + at least one media chunk, got ${chunks}`);

writeFileSync(outFile, Buffer.concat(parts.map((p) => Buffer.from(p))));
console.log(
  `ok: ${chunks} chunks (1 init + ${chunks - 1} media, last ts ${lastTs} ms) -> ${outFile}`,
);
