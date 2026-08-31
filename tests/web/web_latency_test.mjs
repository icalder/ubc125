// Client-side latency harness (BUFFERING-FIXES.md §6.2).
//
// Samples the browser playhead, the buffered lead, and the client chunk queue
// every --sample-ms so a buffering change is measured instead of argued about.
// The middle half of the run is throttled (Network.emulateNetworkConditions),
// so the client has to fall behind and come back.
//
// Asserted for any source:
//   * the playhead never stalls for longer than --stall-ms while the state is
//     "playing" — a frozen playhead under a "playing" label is the silent
//     audio-loss failure mode (B8 gap-skip exists to prevent it);
//   * no `reconnecting` in the sample window (B5 drop-oldest is policy: the
//     client keeps its MediaSource, it does not cycle).
//
// Reported always, asserted only with --max-lead-p99=S: the buffered lead
// `buffered.end(last) - currentTime`. A lead number is only meaningful against
// a real-time source — the `audio-tone --loop` fixture appends ~250x faster
// than playback, so its lead grows by construction and says nothing about the
// scanner. For the numbers §1 quotes, run against the Pi (`serve --device …`,
// `--no-stack`) where §6's p50 ≤ 0.3 s / p99 ≤ 0.6 s applies.
//
// Prereqs: Edge launched by the browser-tools skill (CDP on :9222) and a debug
// build of the binary. Without --no-stack the fake stack is started here with
// the looping tone source (as web_audio_test.mjs does).
//
//   node tests/web/web_latency_test.mjs --seconds 60 --throttle-kbps 64
//   node tests/web/web_latency_test.mjs --no-stack --max-lead-p99 0.6
import { execSync } from "child_process";
import { createRequire } from "module";
import { fileURLToPath } from "url";
const require = createRequire(
  "/home/itcalde/.pi/agent/skills/browser-tools/package.json",
);
const puppeteer = require("puppeteer-core");

const ROOT = fileURLToPath(new URL("../..", import.meta.url)); // repo root

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i === -1 ? fallback : process.argv[i + 1];
};
const flag = (name) => process.argv.includes(`--${name}`);

const SECONDS = Number(arg("seconds", 60));
const THROTTLE_KBPS = Number(arg("throttle-kbps", 64));
const SAMPLE_MS = Number(arg("sample-ms", 250));
const STALL_MS = Number(arg("stall-ms", 1500));
const MAX_LEAD_P99 = flag("max-lead-p99")
  ? Number(arg("max-lead-p99", "0.6"))
  : null;

let pass = 0;
let fail = 0;
const ok = (label, want, got, cond) => {
  cond ? pass++ : fail++;
  console.log(`${cond ? "PASS" : "FAIL"}: ${label} (want ${want}; got ${got})`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

if (!flag("no-stack")) {
  console.log("starting stack (tone audio source)...");
  execSync(
    `UBC125_AUDIO_CMD="${ROOT}/target/debug/ubc125 audio-tone --loop --out -" bash ${ROOT}/tests/ubc125_stack.sh`,
    { timeout: 60000 },
  );
}

const browser = await puppeteer.connect({ browserURL: "http://localhost:9222" });
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 720 });
await page.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(1200);

// One read of every number the harness cares about, through the test seam
// (`window.__ubc125.audioStream`; the <audio> element is detached, so
// audibility is not DOM-observable — the playhead is the ground truth).
const sample = () =>
  page.evaluate(() => {
    const stream = window.__ubc125?.audioStream;
    const audio = stream?._audio;
    const sb = stream?._sb;
    return {
      t: audio ? audio.currentTime : null,
      lead:
        audio && sb && sb.buffered.length
          ? sb.buffered.end(sb.buffered.length - 1) - audio.currentTime
          : null,
      ranges: sb ? sb.buffered.length : 0,
      queued: stream?._queue?.size ?? null,
      readyState: audio ? audio.readyState : null,
      state: stream?.state ?? null,
      // Client-side policy counters: seeks over dropped-chunk holes, and
      // live-edge jumps (audio deliberately discarded to stay near live).
      gapSkips: stream?._gapSkips ?? null,
      catchups: stream?._catchups ?? null,
    };
  });

const play = await page.evaluateHandle(
  () => document.querySelector('button.btn[data-key="p"]:not(:disabled)'),
);
await play.click();
await sleep(3000);

const runStart = Date.now();
const runEnd = runStart + SECONDS * 1000;
const throttleFrom = runStart + (SECONDS * 1000) / 4;
const throttleTo = runStart + (3 * SECONDS * 1000) / 4;
const rows = [];
let throttled = false;
while (Date.now() < runEnd) {
  const wantThrottle = Date.now() >= throttleFrom && Date.now() < throttleTo;
  if (wantThrottle !== throttled) {
    throttled = wantThrottle;
    console.log(
      throttled
        ? `--- throttling to ${THROTTLE_KBPS} kbps ---`
        : "--- network clean again ---",
    );
    await page.emulateNetworkConditions(
      throttled
        ? { latency: 0, download: THROTTLE_KBPS * 1024, upload: 8 * 1024 }
        : null,
    );
  }
  const row = await sample();
  row.at = Date.now();
  row.throttled = wantThrottle;
  rows.push(row);
  await sleep(SAMPLE_MS);
}
await page.emulateNetworkConditions(null);

const pct = (values, p) => {
  const sorted = [...values].sort((a, b) => a - b);
  if (sorted.length === 0) return null;
  const i = Math.min(sorted.length - 1, Math.round((p / 100) * sorted.length) - 1);
  return sorted[Math.max(0, i)];
};
const fmt = (v, digits = 2) => (v === null ? "-" : v.toFixed(digits));

// Longest run of consecutive samples in which the playhead did not move while
// the state said "playing".
let worstStallMs = 0;
let stallStart = null;
for (let i = 1; i < rows.length; i++) {
  const prev = rows[i - 1];
  const cur = rows[i];
  const stalled =
    cur.state === "playing" && prev.t !== null && cur.t <= prev.t + 0.001;
  if (!stalled) {
    stallStart = null;
    continue;
  }
  stallStart ??= prev.at;
  worstStallMs = Math.max(worstStallMs, cur.at - stallStart + SAMPLE_MS);
}

const states = [];
for (const row of rows) {
  if (row.state && (states.length === 0 || states.at(-1) !== row.state)) {
    states.push(row.state);
  }
}
const segment = (label, isThrottled) => {
  const inSegment = rows.filter((r) => r.throttled === isThrottled);
  const leads = inSegment.filter((r) => r.lead !== null).map((r) => r.lead);
  const queued = inSegment.filter((r) => r.queued !== null).map((r) => r.queued);
  console.log(
    `${label.padEnd(10)} n=${String(inSegment.length).padStart(4)}  ` +
      `lead p50=${fmt(pct(leads, 50))}s p90=${fmt(pct(leads, 90))}s p99=${fmt(pct(leads, 99))}s max=${fmt(pct(leads, 100))}s  ` +
      `queued p50=${fmt(pct(queued, 50), 0)} p99=${fmt(pct(queued, 99), 0)}  ` +
      `gapSkips=${Math.max(0, ...inSegment.map((r) => r.gapSkips ?? 0))} ` +
      `catchups=${Math.max(0, ...inSegment.map((r) => r.catchups ?? 0))}  ` +
      `ranges max=${Math.max(0, ...inSegment.map((r) => r.ranges))}`,
  );
  return pct(leads, 99);
};

console.log(`samples: ${rows.length} over ${SECONDS}s (every ${SAMPLE_MS} ms)`);
const cleanP99 = segment("clean", false);
const throttledP99 = segment("throttled", true);
const playing = rows.filter((r) => r.state === "playing").length;
console.log(`states seen: ${states.join(" -> ") || "(none)"}  playing: ${Math.round((100 * playing) / rows.length)}%`);
console.log(`worst playhead stall: ${worstStallMs} ms (limit ${STALL_MS} ms)`);

ok(
  "playhead never stalls (B8 gap-skip)",
  `stall <= ${STALL_MS} ms`,
  `${worstStallMs} ms`,
  worstStallMs <= STALL_MS,
);
ok(
  "no reconnect churn (B5 drop-oldest keeps the stream)",
  "no 'reconnecting'",
  states.join(" -> ") || "(none)",
  !states.includes("reconnecting"),
);
ok(
  "audio was playing for most of the run",
  ">= 80% samples playing",
  `${Math.round((100 * playing) / rows.length)}%`,
  playing >= 0.8 * rows.length,
);
if (MAX_LEAD_P99 !== null) {
  for (const [label, p99] of [["clean", cleanP99], ["throttled", throttledP99]]) {
    ok(
      `${label} buffered lead p99`,
      `<= ${MAX_LEAD_P99} s`,
      `${fmt(p99)} s`,
      p99 !== null && p99 <= MAX_LEAD_P99,
    );
  }
} else {
  console.log(
    "lead not asserted (unpaced fixture); pass --max-lead-p99=0.6 on a paced source",
  );
}

await page.close();
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
