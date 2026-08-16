import { test } from "node:test";
import assert from "node:assert/strict";
import { fromUserInput, toDisplay, isEmpty } from "../lib/freq.js";

test("fromUserInput: MHz.KHz form", () => {
  assert.equal(fromUserInput("123.9750"), "01239750");
  assert.equal(fromUserInput("118.100"), "01181000");
  assert.equal(fromUserInput("88.1"), "00881000");
  assert.equal(fromUserInput("150.0000"), "01500000");
});

test("fromUserInput: partial dot forms", () => {
  assert.equal(fromUserInput(".1"), "00001000");
  assert.equal(fromUserInput("123."), "01230000");
  assert.equal(fromUserInput("0.5"), "00005000");
});

test("fromUserInput: raw and short forms", () => {
  assert.equal(fromUserInput("01239750"), "01239750");
  assert.equal(fromUserInput("1239750"), "01239750"); // 7 digits, left-padded
  assert.equal(fromUserInput("123"), "01230000"); // short MHz
  assert.equal(fromUserInput("9"), "00090000");
});

test("fromUserInput: whitespace trimmed", () => {
  assert.equal(fromUserInput("  123.456  "), "01234560");
});

test("fromUserInput: rejects invalid input", () => {
  assert.equal(fromUserInput(""), null);
  assert.equal(fromUserInput("   "), null);
  assert.equal(fromUserInput("."), null);
  assert.equal(fromUserInput("1.2.3"), null);
  assert.equal(fromUserInput("12a.3"), null);
  assert.equal(fromUserInput("-123"), null);
  assert.equal(fromUserInput("12345.6"), null); // 5 MHz digits
  assert.equal(fromUserInput("12.34567"), null); // 5 KHz digits
  assert.equal(fromUserInput("123456789"), null); // 9 raw digits
  assert.equal(fromUserInput("12345"), null); // 5-digit "MHz" overflows
  assert.equal(fromUserInput("123456"), null); // 6-digit "MHz" overflows
});

test("fromUserInput: boundary values", () => {
  assert.equal(fromUserInput("0.0"), "00000000");
  assert.equal(fromUserInput("9999.9999"), "99999999");
  assert.equal(fromUserInput("10000"), null); // 10000.0000 rejected
});

test("toDisplay: strips MHz leading zeros, keeps KHz", () => {
  assert.equal(toDisplay("01239750"), "123.9750");
  assert.equal(toDisplay("00881000"), "88.1000");
  assert.equal(toDisplay("01500000"), "150.0000");
  assert.equal(toDisplay("00001000"), "0.1000");
  assert.equal(toDisplay(1239750), "123.9750"); // number input
});

test("toDisplay: empty", () => {
  assert.equal(toDisplay("00000000"), "");
  assert.equal(toDisplay(0), "");
  assert.equal(toDisplay(""), "");
  assert.equal(toDisplay("garbage"), "");
});

test("isEmpty", () => {
  assert.ok(isEmpty("00000000"));
  assert.ok(isEmpty(0));
  assert.ok(isEmpty(""));
  assert.ok(!isEmpty("01239750"));
});
