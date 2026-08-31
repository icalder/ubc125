// Audio browser E2E.
//
// Three phases against the fake-scanner stack with D1 audio sources:
//
//   A. Deterministic/offline: UBC125_AUDIO_CMD = tests/paced_file.py over
//      /tmp/cap.webm (a finite WebM/Opus file, streamed over ~4 s — a bare
//      `cat` would dump the whole file in one burst, so a client with the
//      8-slot subscriber queue would only ever see its last chunks before
//      the generation ended). Play -> connecting
//      -> playing; file ends -> the generation ends cleanly ->
//      reconnecting; the next generation replays from the top (fresh init,
//      fresh MediaSource) -> playing again. Stop releases the source
//      process.
//
//   B. Continuous + throttled client: UBC125_AUDIO_CMD = `ubc125
//      audio-tone --loop` (faster than real time, so a slow client cannot
//      keep up). With a 64 KB/s downlink the subscriber's queue fills and
//      the server drops the oldest chunks (drop-oldest, B5) instead of
//      ending the stream, so the client stays "playing" and there is no
//      reconnect cycle — the audio it lost is gone.
//      The dropped chunks leave holes in the buffered timeline, and
//      MediaSource stops the playhead at a hole instead of noticing: the
//      label keeps saying "playing" over silent audio (measured: frozen at
//      t=9.29 s for 28 of 39 500 ms samples before B8 existed). So phase B
//      asserts two things — no reconnect, and the playhead still advancing
//      while throttled (B8 gap-skip, see gapSkip in lib/audio.js).
//
//   C. Late joiner (second browser): a client that joins a running
//      generation receives clusters whose timecodes begin at the
//      generation's elapsed time; without the late-join seek its playhead
//      stalls at 0 — silent, forever — while the label says "playing".
//      The playhead is the ground truth (via the window.__ubc125 test
//      seam): it must be seeked into the stream and then advance, and the
//      first tab must be undisturbed.
//
// Prereqs: Edge launched by the browser-tools skill (CDP on :9222) and a
// debug build of the binary (cargo build --bins); the stack script
// self-provisions socat if needed.
//   node tests/web/web_audio_test.mjs

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

// Play/Stop render as SVG icons, so the buttons are selected by their
// data-key attribute (the scanner key label), not by visible text.
const clickBtn = (key) =>
  p.evaluate((k) => {
    const e = document.querySelector(`button.btn[data-key="${k}"]`);
    if (!e || e.disabled) return false;
    e.click();
    return true;
  }, key);

const btnEnabled = (key) =>
  p.evaluate((k) => {
    const e = document.querySelector(`button.btn[data-key="${k}"]`);
    return e ? !e.disabled : null;
  }, key);

const pgrep = (pattern) =>
  execSync(`pgrep -f '${pattern}' || true`).toString().trim();

// -- phase A: deterministic file source --------------------------------------

// 60 s of tone from the same muxer the Pi capture uses (the file streams
// into the server in a second or two, long enough for "playing" to be
// observable, then ends -> exercises the reconnect/replay cycle).
//
// The file must keep the streaming byte shape: the muxer always writes the
// Segment element with an unknown size (it cannot seek back to patch it
// in), which is what the segmenter and Chrome's MSE expect (a finite
// Segment size is rejected by both).
const bin = `${root}/target/debug/ubc125`;
execSync(
  `rm -f /tmp/cap.webm; ${bin} audio-tone --out /tmp/cap.webm --duration 60`,
  { stdio: "inherit", shell: "/bin/bash" },
);
// Pace the file over ~4 s: a bare `cat` would dump it in milliseconds and
// the generation would end before the client's first poll. Paced, the
// faster-than-real-time stream ends well inside the wait windows below.
// (With drop-oldest, B5, a fast source no longer kills the stream — it
// discards stale chunks — so pacing is a timing convenience, not a
// liveness requirement.)
console.log("starting stack (paced file audio source)...");
execSync(
  `UBC125_AUDIO_CMD="python3 ${root}/tests/paced_file.py /tmp/cap.webm 4" bash ${stack}`,
  { timeout: 60000 },
);

await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(1500);

ok((await audioState()) === "off", "audio defaults to off");
ok((await btnEnabled("x")) === false, "Stop disabled while off");
ok((await btnEnabled("p")) === true, "Play enabled while off");

ok(await clickBtn("p"), "Play clicked");
const seenA = await watchAudio("playing", 15000);
ok(seenA.includes("playing"), `reaches playing (saw: ${seenA.join(" -> ")})`);
ok(await btnEnabled("x"), "Stop enabled while playing");
ok(!(await btnEnabled("p")), "Play disabled while playing");

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

ok(await clickBtn("x"), "Stop clicked");
const seenStop = await watchAudio("off", 5000);
ok(seenStop.includes("off"), `stop -> off (saw: ${seenStop.join(" -> ")})`);
await sleep(500);
ok(
  pgrep("paced_file[.]py") === "",
  "no leftover audio source process after Stop"
);

// -- phase B: continuous source, throttled client ----------------------------

console.log("starting stack (tone audio source)...");
execSync(
  `UBC125_AUDIO_CMD="${bin} audio-tone --loop --out -" bash ${stack}`,
  { timeout: 60000 },
);
await p.reload({ waitUntil: "networkidle2" });
await sleep(1500);

// 64 KB/s downlink: the faster-than-real-time tone source outruns the
// subscriber's 8-slot queue. With drop-oldest (B5) the server keeps the
// stream alive and discards stale chunks instead of ending it, so the
// client must stay "playing" (audio is dropped, not a reconnect cycle).
await p.emulateNetworkConditions({
  latency: 0,
  download: 64 * 1024,
  upload: 8 * 1024,
});
// The playhead, sampled around the throttled window (B8's proof).
const playhead = () =>
  p.evaluate(() => {
    const a = window.__ubc125?.audioStream?._audio;
    return a ? a.currentTime : null;
  });
const tBefore = await playhead();
ok(await clickBtn("p"), "Play clicked (throttled)");
const seenThrottled = await watchAudio("playing", 30000);
ok(
  seenThrottled.includes("playing"),
  `throttled client drops audio but stays playing (saw: ${seenThrottled.join(" -> ")})`,
);
ok(!seenThrottled.includes("unavailable"), "never 'unavailable' (codec is supported)");
// Sample the state for a window: the old behavior cycled through
// "reconnecting" within a few seconds of the backlog building, so a clean
// window proves the stream stayed alive (B5).
const seenWindow = [];
const winEnd = Date.now() + 12000;
while (Date.now() < winEnd) {
  const s = await audioState();
  if (s && (seenWindow.length === 0 || seenWindow[seenWindow.length - 1] !== s)) seenWindow.push(s);
  await sleep(300);
}
ok(
  !seenWindow.includes("reconnecting"),
  `no reconnect churn while throttled (saw: ${seenWindow.join(" -> ")})`,
);
const tDuring = await playhead();
// A frozen playhead under a "playing" label is silent audio: MediaSource
// stopped it at a hole left by the dropped chunks and nothing skipped it.
const advanced =
  tBefore !== null && tDuring !== null && tDuring > tBefore + 1;
ok(
  "playhead advances while throttled (B8 gap-skip, not a silent freeze)",
  "t advanced > 1s during the throttle window",
  `t ${tBefore?.toFixed(2)} -> ${tDuring?.toFixed(2)}`,
  advanced,
);

// Unthrottle: playback should keep flowing (still playing).
await p.emulateNetworkConditions(null);
await sleep(2000);
const sAfter = await audioState();
ok(sAfter === "playing", `still playing after unthrottle (saw: ${sAfter})`);
// -- phase C: late joiner (second browser) -----------------------------------

const p2 = await b.newPage();
await p2.setViewport({ width: 1280, height: 720 });
const audioState2 = async () =>
  p2.$eval(".audio-state", (e) => e.textContent.trim()).catch(() => null);
const watchAudio2 = async (want, timeoutMs) => {
  const seen = [];
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const s = await audioState2();
    if (s && (seen.length === 0 || seen[seen.length - 1] !== s)) seen.push(s);
    if (s === want) return seen;
    await sleep(150);
  }
  return seen;
};
await p2.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(1500);

// Trusted click (CDP input, not the page's untrusted .click()) so the
// autoplay policy really starts the element: the playhead checks below
// are meaningless for a paused element.
const play2 = await p2.evaluateHandle(
  () => document.querySelector('button.btn[data-key="p"]:not(:disabled)')
);
await play2.click();
const seenLate = await watchAudio2("playing", 15000);
ok(
  seenLate.includes("playing"),
  `late joiner reaches playing (saw: ${seenLate.join(" -> ")})`,
);
ok(
  (await audioState()) === "playing",
  "original tab still playing after the late joiner joined",
);

const playhead2 = () =>
  p2.evaluate(() => {
    const a = window.__ubc125?.audioStream?._audio;
    return a ? a.currentTime : null;
  });
let t1 = null;
const seekDeadline = Date.now() + 10000;
while (Date.now() < seekDeadline) {
  t1 = await playhead2();
  if (t1 !== null && t1 > 0) break;
  await sleep(200);
}
ok(
  t1 !== null && t1 > 0,
  `late joiner playhead seeked into the stream (t=${t1}s)`,
);
await sleep(600);
const t2 = await playhead2();
ok(t2 > t1, `late joiner playhead advances (t1=${t1}s, t2=${t2}s)`);

// Before the Stop: a live tab 2 would re-subscribe to the next generation
// and keep a tone source alive, failing the leftover-process check below.
await p2.close();

ok(await clickBtn("x"), "Stop clicked (unthrottled)");
await watchAudio("off", 5000);
await sleep(500);
// Bracket trick: the pattern must not appear literally in the command line
// of the shell execSync spawns, or pgrep matches that shell itself.
ok(pgrep("audio-tone --l[o]op") === "", "no leftover tone source after Stop");

await p.close();
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
