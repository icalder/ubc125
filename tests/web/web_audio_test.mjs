// Audio browser E2E (AUDIO-IMPL 8.4).
//
// Two phases against the fake-scanner stack with D1 audio sources:
//
//   A. Deterministic/offline: UBC125_AUDIO_CMD = tests/paced_file.py over
//      /tmp/cap.webm (a finite WebM/Opus file, streamed over ~4 s — a bare
//      `cat` would outpace the 64-slot broadcast channel and the stream
//      would die with Lagged before the first chunk). Play -> connecting
//      -> playing; file ends -> the generation ends cleanly ->
//      reconnecting; the next generation replays from the top (fresh init,
//      fresh MediaSource) -> playing again. Stop releases the source
//      process.
//
//   B. Continuous + throttled client: UBC125_AUDIO_CMD = ffmpeg lavfi sine
//      (faster than real time, so a slow client cannot keep up). With a
//      64 KB/s downlink the server-side broadcast channel lags, the stream
//      ends as an error, and the client cycles through "reconnecting".
//      Unthrottled, a generation flows to "playing".
//
// Prereqs: Edge launched by the browser-tools skill (CDP on :9222);
// ffmpeg on the PATH of the shell running this script (for the file and the
// sine source) — e.g.
//   nix-shell -p socat ffmpeg --run 'node tests/web/web_audio_test.mjs'
// (the stack script self-provisions socat if needed).

import { execSync } from "child_process";
import { fileURLToPath } from "url";
import { createRequire } from "module";
const require = createRequire("/home/itcalde/.pi/agent/skills/browser-tools/package.json");
const puppeteer = require("puppeteer-core");

const root = fileURLToPath(new URL("../..", import.meta.url));
const stack = `${root}/tests/ubc125_stack.sh`;

let pass = 0, fail = 0;
const ok = (cond, label) => {
  cond ? pass++ : fail++;
  console.log(`${cond ? "PASS" : "FAIL"}: ${label}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const b = await puppeteer.connect({ browserURL: "http://localhost:9222" });
// A fresh tab: a previous tab may hold a wedged renderer (e.g. from an
// interrupted run) and reuse would hang every CDP call.
const p = await b.newPage();
await p.setViewport({ width: 1280, height: 720 });

const audioState = async () =>
  p.$eval(".audio-state", (e) => e.textContent.trim()).catch(() => null);

/** Poll the audio state until `want` is seen (or timeout). Returns the
 *  distinct states observed, in order. */
const watchAudio = async (want, timeoutMs) => {
  const seen = [];
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const s = await audioState();
    if (s && (seen.length === 0 || seen[seen.length - 1] !== s)) seen.push(s);
    if (s === want) return seen;
    await sleep(150);
  }
  return seen;
};

const clickBtn = (label) =>
  p.evaluate((t) => {
    const e = [...document.querySelectorAll("button.btn")].find(
      (x) => x.textContent.includes(t) && !x.disabled,
    );
    if (!e) return false;
    e.click();
    return true;
  }, label);

const btnEnabled = (label) =>
  p.evaluate((t) => {
    const e = [...document.querySelectorAll("button.btn")].find(
      (x) => x.textContent.includes(t),
    );
    return e ? !e.disabled : null;
  }, label);

const pgrep = (pattern) =>
  execSync(`pgrep -f '${pattern}' || true`).toString().trim();

// -- phase A: deterministic file source --------------------------------------

// 60 s of tone (same shape as the Pi capture; the file streams into the
// server in a second or two, long enough for "playing" to be observable,
// then ends -> exercises the reconnect/replay cycle).
//
// The file must be the exact `pipe:1` byte stream: written through a pipe,
// ffmpeg cannot seek back, so the Segment element keeps its unknown size
// (a finite Segment size — what a seekable file output produces — is
// rejected by the segmenter and by Chrome's MSE alike).
execSync(
  'rm -f /tmp/cap.webm; ffmpeg -nostdin -hide_banner -loglevel error -f lavfi -i sine=f=440:duration=60 -ar 16000 -ac 1 -c:a libopus -f webm -cluster_time_limit 200 pipe:1 > /tmp/cap.webm',
  { stdio: "inherit", shell: "/bin/bash" },
);
// Pace the file over ~4 s: `cat` would dump it in milliseconds and the
// 64-slot broadcast channel would overflow before the client's first poll
// (Lagged before the first chunk; the stream dies with zero data). Paced,
// the faster-than-real-time stream is slow enough to keep up, and it ends
// well inside the wait windows below.
console.log("starting stack (paced file audio source)...");
execSync(
  `UBC125_AUDIO_CMD="python3 ${root}/tests/paced_file.py /tmp/cap.webm 4" bash ${stack}`,
  { timeout: 60000 },
);

await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(1500);

ok((await audioState()) === "off", "audio defaults to off");
ok((await btnEnabled("Stop")) === false, "Stop disabled while off");
ok((await btnEnabled("Play")) === true, "Play enabled while off");

ok(await clickBtn("p: Play"), "Play clicked");
const seenA = await watchAudio("playing", 15000);
ok(seenA.includes("playing"), `reaches playing (saw: ${seenA.join(" -> ")})`);
ok(await btnEnabled("Stop"), "Stop enabled while playing");
ok(!(await btnEnabled("Play")), "Play disabled while playing");

// The finite file ends: the generation closes cleanly and the client
// reconnects (1 s backoff) into a new generation replaying from the top.
const seenCycle = await watchAudio("reconnecting", 20000);
ok(
  seenCycle.includes("reconnecting"),
  `file end -> reconnecting (saw: ${seenCycle.join(" -> ")})`,
);
const seenReplay = await watchAudio("playing", 15000);
ok(
  seenReplay.includes("playing"),
  `next generation replays -> playing again (saw: ${seenReplay.join(" -> ")})`,
);

ok(await clickBtn("x: Stop"), "Stop clicked");
const seenStop = await watchAudio("off", 5000);
ok(seenStop.includes("off"), `stop -> off (saw: ${seenStop.join(" -> ")})`);
await sleep(500);
ok(
  pgrep("paced_file[.]py") === "",
  "no leftover audio source process after Stop"
);

// -- phase B: continuous source, throttled client ----------------------------

console.log("starting stack (sine audio source)...");
execSync(
  `UBC125_AUDIO_CMD="ffmpeg -nostdin -hide_banner -loglevel error -f lavfi -i sine=f=440 -ar 16000 -ac 1 -c:a libopus -f webm -cluster_time_limit 200 pipe:1" bash ${stack}`,
  { timeout: 60000 },
);
await p.reload({ waitUntil: "networkidle2" });
await sleep(1500);

// 64 KB/s downlink: the faster-than-real-time sine source outruns the
// 64-slot broadcast channel, the server ends the stream as an error, and
// the client must cycle through reconnecting.
await p.emulateNetworkConditions({
  latency: 0,
  download: 64 * 1024,
  upload: 8 * 1024,
});
ok(await clickBtn("p: Play"), "Play clicked (throttled)");
const seenThrottled = await watchAudio("reconnecting", 45000);
ok(
  seenThrottled.includes("reconnecting"),
  `throttled client lags -> reconnecting (saw: ${seenThrottled.join(" -> ")})`,
);
ok(!seenThrottled.includes("unavailable"), "never 'unavailable' (codec is supported)");

// Unthrottle: the next generation should flow to playing.
await p.emulateNetworkConditions(null);
const seenUnthrottled = await watchAudio("playing", 20000);
ok(
  seenUnthrottled.includes("playing"),
  `unthrottled -> playing (saw: ${seenUnthrottled.join(" -> ")})`,
);
ok(await clickBtn("x: Stop"), "Stop clicked (unthrottled)");
await watchAudio("off", 5000);
await sleep(500);
// Bracket trick: the pattern must not appear literally in the command line
// of the shell execSync spawns, or pgrep matches that shell itself.
ok(pgrep("sine=f=[4]40") === "", "no leftover ffmpeg after Stop");

await p.close();
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
