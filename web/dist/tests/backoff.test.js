import { test } from "node:test";
import assert from "node:assert/strict";
import { INITIAL_BACKOFF, MAX_BACKOFF, nextBackoff, backoffSequence } from "../lib/backoff.js";

test("nextBackoff: doubles until the cap", () => {
  assert.equal(nextBackoff(1000), 2000);
  assert.equal(nextBackoff(2000), 4000);
  assert.equal(nextBackoff(16000), 30000);
  assert.equal(nextBackoff(30000), 30000);
});

test("nextBackoff: custom cap", () => {
  assert.equal(nextBackoff(9000, 15000), 15000);
});

test("backoffSequence: 1s -> 30s per PLAN §4.2", () => {
  assert.equal(INITIAL_BACKOFF, 1000);
  assert.equal(MAX_BACKOFF, 30000);
  assert.deepEqual(
    backoffSequence(INITIAL_BACKOFF, 7),
    [1000, 2000, 4000, 8000, 16000, 30000, 30000],
  );
});

test("backoffSequence: stays at the cap once reached", () => {
  const seq = backoffSequence(1000, 10);
  assert.equal(seq.at(-1), 30000);
  assert.ok(seq.every((d) => d >= 1000 && d <= 30000));
});
