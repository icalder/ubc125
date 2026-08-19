// KI-2 regression: two browser tabs against one serve must both stay
// ONLINE. Pre-fix, GetStatus ran a singleton poller — each new stream
// cancelled the previous one, so two tabs cancelled each other's streams
// in a ping-pong and both cycled the "OFFLINE — waiting for scanner..."
// banner. With the shared status poller (src/status.rs) both tabs keep
// receiving live status for the whole observation window.
//
// Prereq: the fake-scanner stack is running (bash tests/ubc125_stack.sh)
// and Edge is up (browser-tools skill, CDP on :9222).
import { createRequire } from "module";
const require = createRequire("/home/itcalde/.pi/agent/skills/browser-tools/package.json");
const puppeteer = require("puppeteer-core");

const OBSERVE_MS = 20_000;
const b = await puppeteer.connect({ browserURL: "http://localhost:9222" });
let pass = 0, fail = 0;
const ok = (cond, label) => { cond ? pass++ : fail++; console.log(`${cond ? "PASS" : "FAIL"}: ${label}`); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// One eval helper per page: reads the help-bar status line (div.status)
// and the offline-banner text.
const readUi = (p) =>
  p.evaluate(() => {
    const statusEl = [...document.querySelectorAll("div")].find((e) =>
      e.className.split(" ").includes("status"),
    );
    return {
      status: statusEl ? statusEl.textContent : "",
      banner: (document.querySelector(".offline-banner")?.textContent ?? "").trim(),
    };
  });
const hasLiveStatus = (ui) => ui.status.includes("GLG,");
const offline = (ui) => ui.banner.length > 0;

const pages = [];
try {
  for (let i = 1; i <= 2; i++) {
    const p = await b.newPage();
    await p.setViewport({ width: 1280, height: 720 });
    await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2", timeout: 30_000 });
    pages.push(p);
    ok((await readUi(p)).status.length > 0 || (await p.$("body")) !== null, `tab ${i} loaded`);
  }
  await sleep(2000);

  // Both tabs must reach live status within the wait window (the second
  // tab joining must not starve the first).
  const deadline = Date.now() + 15_000;
  for (let i = 0; i < 2; i++) {
    let ui;
    while (Date.now() < deadline) {
      ui = await readUi(pages[i]);
      if (hasLiveStatus(ui)) break;
      await sleep(250);
    }
    ui = await readUi(pages[i]);
    ok(hasLiveStatus(ui), `tab ${i + 1} shows live GLG status`);
  }

  // The KI-2 check: observe both tabs; the OFFLINE banner must never
  // appear while the other tab is connected.
  const bannerSamples = [0, 0];
  const t0 = Date.now();
  while (Date.now() - t0 < OBSERVE_MS) {
    for (let i = 0; i < 2; i++) {
      if (offline(await readUi(pages[i]))) bannerSamples[i]++;
    }
    await sleep(250);
  }
  ok(bannerSamples[0] === 0, `tab 1 never went OFFLINE during ${OBSERVE_MS / 1000}s (saw ${bannerSamples[0]} banner samples)`);
  ok(bannerSamples[1] === 0, `tab 2 never went OFFLINE during ${OBSERVE_MS / 1000}s (saw ${bannerSamples[1]} banner samples)`);

  // And both still show live status at the end (streams still flowing).
  for (let i = 0; i < 2; i++) {
    ok(hasLiveStatus(await readUi(pages[i])), `tab ${i + 1} still live at end of observation`);
  }

  console.log(`\n${pass} pass, ${fail} fail (two tabs, ${OBSERVE_MS / 1000}s observation)`);
  await pages[0].screenshot({ path: "/tmp/ki2-two-tabs.png" });
} finally {
  for (const p of pages) await p.close().catch(() => {});
  b.disconnect();
  process.exit(fail ? 1 : 0);
}
