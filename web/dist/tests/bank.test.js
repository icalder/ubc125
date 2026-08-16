import { test } from "node:test";
import assert from "node:assert/strict";
import { bankLabel } from "../views/monitor.js";
import { bankRange, CHANNELS_PER_BANK } from "../views/bank.js";

test("bankLabel: banks 1-9 show [1]-[9]", () => {
  for (let bank = 1; bank <= 9; bank++) {
    assert.equal(bankLabel(bank), `[${bank}]`);
  }
});

test("bankLabel: bank 10 shows [0] (console bank_num % 10 quirk)", () => {
  assert.equal(bankLabel(10), "[0]");
});

test("bankRange: inclusive 1-based channel ranges", () => {
  assert.deepEqual(bankRange(1), [1, 50]);
  assert.deepEqual(bankRange(2), [51, 100]);
  assert.deepEqual(bankRange(10), [451, 500]);
});

test("bankRange: each bank covers CHANNELS_PER_BANK channels", () => {
  for (let bank = 1; bank <= 10; bank++) {
    const [start, end] = bankRange(bank);
    assert.equal(end - start + 1, CHANNELS_PER_BANK);
    assert.equal(start, (bank - 1) * CHANNELS_PER_BANK + 1);
  }
});

test("cursor-in-bank: 0-based index = cursor - bankRange start", () => {
  const [start] = bankRange(2);
  assert.equal(51 - start, 0); // first row of bank 2
  assert.equal(100 - start, 49); // last row of bank 2
  const [start10] = bankRange(10);
  assert.equal(451 - start10, 0);
  assert.equal(500 - start10, 49);
});
