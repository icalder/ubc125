import { createRequire } from "module";
import { execSync, spawn } from "child_process";
const require = createRequire("/home/itcalde/.pi/agent/skills/browser-tools/package.json");
const puppeteer = require("puppeteer-core");

const b = await puppeteer.connect({ browserURL: "http://localhost:9222" });
const p = (await b.pages()).at(-1);
let pass = 0, fail = 0;
const ok = (cond, label) => { cond ? pass++ : fail++; console.log(`${cond ? "PASS" : "FAIL"}: ${label}`); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const banner = () => p.evaluate(() => !!document.querySelector(".offline-banner"));
const fakePid = () => parseInt(execSync("pgrep -f 'fake_sc[a]nner'").toString().trim().split("\n")[0]);

// --- offline: stop the server ~4s → banner → restart → recovers -----------
// (server keeps the stream alive through transient poll errors by design,
// so the banner's trigger is the connection going down, not a GLG hiccup)
await p.setViewport({ width: 1280, height: 720 });
await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(2500);
ok(!(await banner()), "no banner while healthy");

process.stdout.write("stopping server 4s... ");
execSync("pgrep -x ubc125 | xargs -r kill");
let sawBanner = false;
const end1 = Date.now() + 30000;
while (Date.now() < end1 && !sawBanner) { sawBanner = await banner(); await sleep(500); }
ok(sawBanner, "offline banner appears while server down");
if (sawBanner) await p.screenshot({ path: "/tmp/wt-offline-banner.png" });

await sleep(4000);
process.stdout.write("restarting stack... ");
execSync(
  "pgrep -x ubc125 | xargs -r kill; pgrep -x socat | xargs -r kill; " +
  "pgrep -f 'fake_sc[a]nner' | xargs -r kill; sleep 1; bash /tmp/ubc125_stack.sh",
  { timeout: 60000 },
);
const end2 = Date.now() + 40000;
let recovered = false;
while (Date.now() < end2 && !recovered) { recovered = !(await banner()); await sleep(1000); }
ok(recovered, "banner clears after stack restart");
await sleep(1500);

// --- phone viewport 390x844 -----------------------------------------------
await p.setViewport({ width: 390, height: 844, isMobile: true, hasTouch: true });
await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(2500);
const layout = await p.evaluate(() => {
  const tabs = document.querySelector(".tabbar") || document.querySelector("#app section");
  const table = document.querySelector(".table");
  const btns = [...document.querySelectorAll("button.btn")];
  const minBtnH = Math.min(...btns.map((b) => b.getBoundingClientRect().height));
  return {
    docW: document.documentElement.scrollWidth,
    winW: innerWidth,
    tableRows: table ? table.querySelectorAll(".row").length : 0,
    minBtnH,
    bodyOverflowX: document.documentElement.scrollWidth > innerWidth + 1,
  };
});
ok(!layout.bodyOverflowX, `no horizontal overflow at 390px (doc ${layout.docW} vs win ${layout.winW})`);
ok(layout.minBtnH >= 44, `action buttons >= 44px (got ${layout.minBtnH})`);

// switch to Bank 1 at phone size, check 50 rows + tap a row
await p.evaluate(() => [...document.querySelectorAll(".tab")].find((t) => t.textContent.includes("Bank 1")).click());
await sleep(1200);
const phone = await p.evaluate(() => ({
  rows: document.querySelectorAll(".table .row").length - 1,
  firstIdx: document.querySelector(".table .row:not(.header) .col.idx")?.textContent.trim(),
  docW: document.documentElement.scrollWidth,
  winW: innerWidth,
}));
ok(phone.rows === 50, `bank table 50 rows at 390px (got ${phone.rows})`);
ok(phone.firstIdx.replace(">> ", "") === "1", `bank 1 starts at idx 1 (got ${phone.firstIdx})`);
ok(phone.docW <= phone.winW + 1, `no horizontal overflow in bank view (${phone.docW}/${phone.winW})`);

// tap row 5, open edit via button, save unchanged values (round-trip)
await p.evaluate(() => {
  const e = [...document.querySelectorAll(".table .row")].find((r) => r.querySelector(".col.idx")?.textContent.trim().replace(">> ", "") === "5");
  e?.click();
});
await sleep(300);
await p.evaluate(() => [...document.querySelectorAll("button.btn")].find((btn) => btn.textContent.includes("Edit")).click());
await sleep(400);
ok(await p.evaluate(() => !!document.querySelector(".modal-backdrop")), "edit modal opens by tap at 390px");
await p.evaluate(() => [...document.querySelectorAll("button")].find((btn) => btn.textContent.includes("Save")).click());
await sleep(1500);
const saved = await p.evaluate(() => !document.querySelector(".modal-backdrop"));
ok(saved, "save closes modal at 390px");

await p.screenshot({ path: "/tmp/wt-phone-bank.png" });
await p.evaluate(() => [...document.querySelectorAll(".tab")].find((t) => t.textContent.includes("Monitor")).click());
await sleep(500);
await p.screenshot({ path: "/tmp/wt-phone-monitor.png" });

console.log(`\n${pass} pass, ${fail} fail`);
process.exit(fail ? 1 : 0);
