// Bank-sync regression: a scan-bank change made in one tab must appear in
// every other tab connected to the same serve. Pre-fix, `state.banks` is
// loaded once on page load (`GetEnabledBanks`) and only updated by local
// toggles — the `GetStatus` stream carries no bank mask — so a chip change
// in tab 1 never reached tab 2 (stale "Active Banks" chips).
//
// Prereq: the fake-scanner stack is running (bash tests/ubc125_stack.sh)
// and Edge is up (browser-tools skill, CDP on :9222). The fake scanner
// persists the SCG bank mask, so tab 2's (post-fix) re-read of the mask
// would see what tab 1 wrote.
import { createRequire } from "module";
const require = createRequire("/home/itcalde/.pi/agent/skills/browser-tools/package.json");
const puppeteer = require("puppeteer-core");

const SYNC_WAIT_MS = 10_000;
const b = await puppeteer.connect({ browserURL: "http://localhost:9222" });
let pass = 0, fail = 0;
const ok = (cond, label) => { cond ? pass++ : fail++; console.log(`${cond ? "PASS" : "FAIL"}: ${label}`); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// One eval helper per page: help-bar status line (div.status) plus the
// Active Banks chip states (10 booleans; chip order = banks 1..10, so the
// [0]-labelled chip is the last one and is bank 10).
const readUi = (p) =>
  p.evaluate(() => {
    const statusEl = [...document.querySelectorAll("div")].find((e) =>
      e.className.split(" ").includes("status"),
    );
    return {
      status: statusEl ? statusEl.textContent : "",
      banks: [...document.querySelectorAll(".bank-chip")].map((c) => c.classList.contains("on")),
    };
  });
const hasLiveStatus = (ui) => ui.status.includes("GLG,");
const sameMask = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
const maskStr = (m) => m.map((v) => (v ? "1" : "0")).join("");
const clickChip = (p, i) =>
  p.evaluate((n) => { document.querySelectorAll(".bank-chip")[n].click(); }, i);

const pages = [];
try {
  for (let i = 1; i <= 2; i++) {
    const p = await b.newPage();
    await p.setViewport({ width: 1280, height: 720 });
    await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2", timeout: 30_000 });
    pages.push(p);
    const n = (await readUi(p)).banks.length;
    ok(n === 10, `tab ${i} loaded (${n} bank chips)`);
  }

  // Both tabs must be live before the toggle: a dead tab 2 would "fail"
  // the sync check for the wrong reason (that's KI-2, covered by
  // web_two_tabs_test.mjs).
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

  // Both tabs started from the same mask (each freshly read it on load).
  let m1 = (await readUi(pages[0])).banks;
  let m2 = (await readUi(pages[1])).banks;
  ok(sameMask(m1, m2), `initial bank masks agree (tab1=${maskStr(m1)} tab2=${maskStr(m2)})`);

  // Pick a bank that is ON and turn it OFF in tab 1.
  const bank = m1.findIndex((on) => on);
  if (bank < 0) {
    ok(false, "no enabled bank to toggle (all off?)");
  } else {
    await clickChip(pages[0], bank);

    // Tab 1 updates its own chip (local state + SCG write round-trip).
    let flipped = false;
    const d1 = Date.now() + 5000;
    while (Date.now() < d1) {
      m1 = (await readUi(pages[0])).banks;
      if (!m1[bank]) { flipped = true; break; }
      await sleep(150);
    }
    ok(flipped, `tab 1 shows bank ${bank + 1} disabled after its own toggle`);

    // The regression check: tab 2 must converge to tab 1's new mask.
    const d2 = Date.now() + SYNC_WAIT_MS;
    while (Date.now() < d2) {
      m2 = (await readUi(pages[1])).banks;
      if (sameMask(m2, m1)) break;
      await sleep(250);
    }
    m2 = (await readUi(pages[1])).banks;
    ok(sameMask(m2, m1),
      `tab 2 bank mask converged to tab 1's within ${SYNC_WAIT_MS / 1000}s (tab1=${maskStr(m1)} tab2=${maskStr(m2)})`);
    ok(m2[bank] === false, `tab 2 shows bank ${bank + 1} disabled`);

    // Restore the bank so the next run starts from the same mask (the
    // fake keeps the SCG mask across runs).
    await clickChip(pages[0], bank);
    await sleep(500);
    ok((await readUi(pages[0])).banks[bank] === true, `tab 1 shows bank ${bank + 1} enabled again (restored)`);
  }

  // Both tabs still live at the end (whatever syncs the banks must not
  // kill the status streams). Poll: the restore's 3 s flash temporarily
  // replaces the GLG status line.
  for (let i = 0; i < 2; i++) {
    let ui;
    const dEnd = Date.now() + 6000;
    while (Date.now() < dEnd) {
      ui = await readUi(pages[i]);
      if (hasLiveStatus(ui)) break;
      await sleep(250);
    }
    ok(hasLiveStatus(ui), `tab ${i + 1} still live at end`);
  }

  console.log(`\n${pass} pass, ${fail} fail (bank sync, ${SYNC_WAIT_MS / 1000}s sync window)`);
  await pages[1].screenshot({ path: "/tmp/bank-sync-tab2.png" });
} finally {
  for (const p of pages) await p.close().catch(() => {});
  b.disconnect();
  process.exit(fail ? 1 : 0);
}
