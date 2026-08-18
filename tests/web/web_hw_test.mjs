import { createRequire } from "module";
const require = createRequire("/home/itcalde/.pi/agent/skills/browser-tools/package.json");
const puppeteer = require("puppeteer-core");

const b = await puppeteer.connect({ browserURL: "http://localhost:9222" });
// A fresh tab: a previous tab may hold wedged renderer state (e.g. from an
// interrupted run) and reuse would hang every CDP call.
const p = await b.newPage();
await p.setViewport({ width: 1280, height: 720 });
let pass = 0, fail = 0;
const ok = (cond, label) => { cond ? pass++ : fail++; console.log(`${cond ? "PASS" : "FAIL"}: ${label}`); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clickText = (kind, sub) =>
  p.evaluate((k, s) => {
    const e = [...document.querySelectorAll(k)].find((x) => x.textContent.includes(s));
    if (!e) return false;
    e.click();
    return true;
  }, kind, sub);
const clickRow = (idx) =>
  p.evaluate((n) => {
    const e = [...document.querySelectorAll(".table .row")].find((r) => {
      const cell = r.querySelector(".col.idx");
      return cell && cell.textContent.trim().replace(">> ", "") === String(n);
    });
    if (!e) return false;
    e.click();
    return true;
  }, idx);
const rowText = (idx) =>
  p.evaluate((n) => {
    const e = [...document.querySelectorAll(".table .row")].find((r) => {
      const cell = r.querySelector(".col.idx");
      return cell && cell.textContent.trim().replace(">> ", "") === String(n);
    });
    return e ? e.textContent : null;
  }, idx);
const waitFlash = async (sub) => {
  const end = Date.now() + 10000;
  while (Date.now() < end) {
    const t = await p.$eval("#app section:last-of-type", (e) => e.textContent).catch(() => "");
    if (t.includes(sub)) return true;
    await sleep(300);
  }
  return false;
};

const IDX = 63, FREQ = "123.9750", NAME = "BHX RADAR";

await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(3000);
ok((await p.evaluate(() => document.body.textContent)).includes("UBC125XLT"), "model info (real scanner)");
ok((await p.evaluate(() => document.body.textContent)).includes("Frequency:"), "live scan row present");

// Bank 2 tab + table
ok(await clickText(".tab", "Bank 2"), "Bank 2 tab clicked");
await sleep(3000); // ListChannels over serial is slow
ok((await rowText(IDX) || "").includes(NAME), `row ${IDX} shows factory data`);

// 1. round-trip edit: open, save unchanged
ok(await clickRow(IDX), `row ${IDX} clicked`);
await sleep(300);
ok(await clickText("button.btn", "Edit"), "Edit button clicked");
await sleep(500);
ok(await p.evaluate(() => !!document.querySelector(".modal-backdrop")), "edit modal open");
await p.evaluate(() => [...document.querySelectorAll("button")].find((btn) => btn.textContent.includes("Save")).click());
ok(await waitFlash(`Channel ${IDX} saved`), "round-trip save flash");
await sleep(2000);
ok((await rowText(IDX) || "").includes(NAME), "row unchanged after round-trip save");

// 2. delete → verify cleared → restore
ok(await clickRow(IDX), `row ${IDX} re-selected`);
await sleep(300);
ok(await clickText("button.btn", "Delete"), "Delete button clicked");
await sleep(500);
ok(await p.evaluate(() => !!document.querySelector(".modal-backdrop")), "confirm dialog open");
await p.evaluate(() => [...document.querySelectorAll("button")].find((btn) => btn.textContent.includes("Yes")).click());
ok(await waitFlash(`Channel ${IDX} deleted`), "delete flash");
await sleep(2500);
const cleared = await rowText(IDX);
ok(cleared !== null && !cleared.includes(NAME), "row cleared after delete");

ok(await clickRow(IDX), "empty row re-selected for restore");
await sleep(300);
ok(await clickText("button.btn", "Edit"), "Edit button clicked (restore)");
await sleep(500);
// fill frequency + name
const inputs = await p.$$("input.field");
ok(inputs.length === 2, "modal has two inputs");
if (inputs.length === 2) {
  await inputs[0].evaluate((i, v) => { i.value = v; }, FREQ);
  await inputs[1].evaluate((i, v) => { i.value = v; }, NAME);
}
await p.evaluate(() => [...document.querySelectorAll("button")].find((btn) => btn.textContent.includes("Save")).click());
ok(await waitFlash(`Channel ${IDX} saved`), "restore save flash");
await sleep(2500);
const restored = await rowText(IDX);
ok((restored || "").includes(NAME) && (restored || "").includes(FREQ), "channel restored (name + freq)");
await p.screenshot({ path: "/tmp/w6-bank-restored.png" });

// 3. bank toggle round-trip on Monitor
ok(await clickText(".tab", "Monitor"), "Monitor tab clicked");
await sleep(500);
const origBanks = await p.evaluate(() => [...document.querySelectorAll(".bank-chip")].map((c) => c.className.includes("on") || c.className.includes("enabled")));
await p.evaluate(() => document.querySelectorAll(".bank-chip")[0].click()); // bank 1 off
await sleep(1500);
await p.evaluate(() => document.querySelectorAll(".bank-chip")[0].click()); // bank 1 back on
await sleep(1500);
const newBanks = await p.evaluate(() => [...document.querySelectorAll(".bank-chip")].map((c) => c.className.includes("on") || c.className.includes("enabled")));
ok(JSON.stringify(origBanks) === JSON.stringify(newBanks), "bank 1 toggle round-trip (state restored)");

// 4. scan / hold
ok(await clickText("button.btn", "Scan"), "Scan button clicked");
ok(await waitFlash("Scan started"), "Scan flash");
ok(await clickText("button.btn", "Hold"), "Hold button clicked");
ok(await waitFlash("Scan held"), "Hold flash");
await p.screenshot({ path: "/tmp/w6-monitor.png" });

console.log(`\n${pass} pass, ${fail} fail (W6 hardware)`);
await p.close();
process.exit(fail ? 1 : 0);
