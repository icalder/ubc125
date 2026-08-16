import { createRequire } from "module";
const require = createRequire("/home/itcalde/.pi/agent/skills/browser-tools/package.json");
const puppeteer = require("puppeteer-core");

const b = await puppeteer.connect({ browserURL: "http://localhost:9222" });
const p = (await b.pages()).at(-1);
await p.setViewport({ width: 1280, height: 720 });
let pass = 0, fail = 0;
const ok = (cond, label) => { cond ? pass++ : fail++; console.log(`${cond ? "PASS" : "FAIL"}: ${label}`); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const text = (sel) => p.$eval(sel, (e) => e.textContent).catch(() => null);
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
const waitFlash = async (sub) => {
  const end = Date.now() + 5000;
  while (Date.now() < end) {
    const t = await text("#app section:last-of-type") ?? "";
    if (t.includes(sub)) return true;
    await sleep(150);
  }
  return false;
};

await p.goto("http://127.0.0.1:50051/", { waitUntil: "networkidle2" });
await sleep(2500);

ok((await text("body") || "").includes("UBC125XLT"), "model info shows UBC125XLT");
ok((await text("body") || "").includes("Frequency:"), "live scan Frequency row present");

ok(await clickText(".tab", "Bank 2"), "Bank 2 tab clicked");
await sleep(1200);
const idxs = await p.$$eval(".table .row:not(.header) .col.idx", (els) => els.map((e) => e.textContent.trim()));
ok(idxs.length === 50, `bank table has 50 rows (got ${idxs.length})`);
const nums = idxs.map((s) => s.replace(">> ", ""));
ok(nums[0] === "51" && nums[49] === "100", `bank 2 idx labels 51..100 (got ${nums[0]}..${nums[49]})`);

ok(await clickRow(53), "row 53 clicked");
await sleep(300);
ok(await p.$eval(".table .row.selected .col.idx", (e) => e.textContent.includes("53")), "row 53 selected by click");

ok(await clickText("button.btn", "Edit"), "Edit button clicked");
await sleep(300);
const nameInput = await p.$("input.field:not(.freq)");
ok(!!nameInput, "edit modal opened");
if (nameInput) {
  await nameInput.evaluate((i) => { i.value = ""; i.dispatchEvent(new Event("input", { bubbles: true })); });
  await nameInput.type("POINTER TEST");
  await p.evaluate(() => [...document.querySelectorAll("button")].find((btn) => btn.textContent.includes("Save")).click());
  await sleep(500);
  const modalGone = await p.evaluate(() => !document.querySelector(".modal-backdrop"));
  ok(modalGone, "edit modal closed after Save");
}
ok(await waitFlash("Channel 53 saved"), "save flash 'Channel 53 saved'");
await sleep(1200);
const nameOk = await p.evaluate(() => [...document.querySelectorAll(".table .row")].some((r) => {
  const cell = r.querySelector(".col.idx");
  return cell && cell.textContent.trim().replace(">> ", "") === "53" && r.textContent.includes("POINTER TEST");
}));
ok(nameOk, "row 53 shows POINTER TEST after save");

ok(await clickRow(53), "row 53 re-selected");
await sleep(200);
ok(await clickText("button.btn", "Delete"), "Delete button clicked");
await sleep(300);
await p.evaluate(() => [...document.querySelectorAll("button")].find((btn) => btn.textContent.includes("Yes")).click());
ok(await waitFlash("Channel 53 deleted"), "delete flash 'Channel 53 deleted'");
await sleep(1200);
const gone = await p.evaluate(() => ![...document.querySelectorAll(".table .row")].some((r) => {
  const cell = r.querySelector(".col.idx");
  return cell && cell.textContent.trim().replace(">> ", "") === "53" && r.textContent.includes("POINTER TEST");
}));
ok(gone, "row 53 cleared after delete");

ok(await clickText(".tab", "Monitor"), "Monitor tab clicked");
await sleep(400);
const before = await p.evaluate(() => document.querySelector(".bank-chip")?.className);
await p.evaluate(() => document.querySelector(".bank-chip")?.click());
ok(await waitFlash("Bank 1 "), "bank 1 toggle flash");
const after = await p.evaluate(() => document.querySelector(".bank-chip")?.className);
ok(before !== after, "bank chip class changed after tap");
await p.evaluate(() => document.querySelector(".bank-chip")?.click());

ok(await clickText("button.btn", "Scan"), "Scan button clicked");
ok(await waitFlash("Scan started"), "Scan button flash");
ok(await clickText("button.btn", "Hold"), "Hold button clicked");
ok(await waitFlash("Scan held"), "Hold button flash");

console.log(`\n${pass} pass, ${fail} fail (1280x720)`);
await p.screenshot({ path: "/tmp/wt-desktop.png" });
process.exit(fail ? 1 : 0);
