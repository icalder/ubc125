import { test } from "node:test";
import assert from "node:assert/strict";
import {
  AUDIO_MIME,
  AUDIO_STATES,
  audioTransition,
  lateJoinSeek,
  decideJoinKind,
  ChunkQueue,
} from "../lib/audio.js";

// -- constants ---------------------------------------------------------------

test("AUDIO_MIME matches the server's WebM/Opus stream", () => {
  assert.equal(AUDIO_MIME, 'audio/webm; codecs="opus"');
});

test("AUDIO_STATES covers the five UI states", () => {
  assert.deepEqual(AUDIO_STATES, [
    "off",
    "connecting",
    "playing",
    "reconnecting",
    "unavailable",
  ]);
});

// -- state machine -----------------------------------------------------------

test("audioTransition: play starts from off", () => {
  assert.equal(audioTransition("off", "play"), "connecting");
  // Play is not valid from any other state.
  for (const s of ["connecting", "playing", "reconnecting", "unavailable"]) {
    assert.equal(audioTransition(s, "play"), s);
  }
});

test("audioTransition: unsupported is only reachable from off", () => {
  assert.equal(audioTransition("off", "unsupported"), "unavailable");
  for (const s of ["connecting", "playing", "reconnecting"]) {
    assert.equal(audioTransition(s, "unsupported"), s);
  }
});

test("audioTransition: ready moves connecting/reconnecting to playing", () => {
  assert.equal(audioTransition("connecting", "ready"), "playing");
  assert.equal(audioTransition("reconnecting", "ready"), "playing");
  for (const s of ["off", "playing", "unavailable"]) {
    assert.equal(audioTransition(s, "ready"), s);
  }
});

test("audioTransition: error moves active states to reconnecting", () => {
  assert.equal(audioTransition("connecting", "error"), "reconnecting");
  assert.equal(audioTransition("playing", "error"), "reconnecting");
  for (const s of ["off", "reconnecting", "unavailable"]) {
    assert.equal(audioTransition(s, "error"), s);
  }
});

test("audioTransition: stop returns to off from any active state", () => {
  for (const s of ["connecting", "playing", "reconnecting", "unavailable"]) {
    assert.equal(audioTransition(s, "stop"), "off");
  }
  assert.equal(audioTransition("off", "stop"), "off");
});

test("audioTransition: unknown events are no-ops", () => {
  for (const s of AUDIO_STATES) {
    assert.equal(audioTransition(s, "bogus"), s);
  }
});

test("audioTransition: full session and reconnect cycle", () => {
  let s = "off";
  s = audioTransition(s, "play");
  assert.equal(s, "connecting");
  s = audioTransition(s, "error");
  assert.equal(s, "reconnecting");
  s = audioTransition(s, "ready");
  assert.equal(s, "playing");
  s = audioTransition(s, "error");
  assert.equal(s, "reconnecting");
  s = audioTransition(s, "ready");
  assert.equal(s, "playing");
  s = audioTransition(s, "stop");
  assert.equal(s, "off");
});

// -- late joiner seek --------------------------------------------------------

test("lateJoinSeek: a join at the head of the generation needs no seek", () => {
  assert.equal(lateJoinSeek(0), null);
  // Sub-millisecond float noise is still "the head".
  assert.equal(lateJoinSeek(0.0005), null);
});

test("lateJoinSeek: a mid-generation joiner seeks to the earliest data", () => {
  // Joined after 30.2 s of capture: the buffer starts there.
  assert.equal(lateJoinSeek(30.2), 30.2);
  // Even one cluster (200 ms) late, the playhead would stall at 0.
  assert.equal(lateJoinSeek(0.2), 0.2);
});

// -- join-kind decision ------------------------------------------------------

test("decideJoinKind: undecided until the buffer has data", () => {
  assert.equal(decideJoinKind(null, null), null);
});

test("decideJoinKind: first buffered data at 0 is a head join", () => {
  assert.equal(decideJoinKind(null, 0), "head");
  assert.equal(decideJoinKind(null, 0.0005), "head");
});

test("decideJoinKind: first buffered data past the epsilon is a late join", () => {
  assert.equal(decideJoinKind(null, 0.2), "late");
  assert.equal(decideJoinKind(null, 30.2), "late");
});

test("decideJoinKind: the decision is sticky once made (regression)", () => {
  // A head join stays "head" even after head-trims move the buffer start
  // forward; that is what previously misread as a late join and replayed
  // the last ~3 s of audio.
  assert.equal(decideJoinKind("head", 3.1), "head");
  assert.equal(decideJoinKind("head", 29.7), "head");
  assert.equal(decideJoinKind("late", 0), "late");
});

// -- chunk queue -------------------------------------------------------------

const init = { initSegment: true, payload: new Uint8Array([1]) };
const media = (n) => ({ initSegment: false, payload: new Uint8Array([n]) });

test("ChunkQueue: media before init is dropped", () => {
  const q = new ChunkQueue();
  assert.equal(q.push(media(1)), false);
  assert.equal(q.push(media(2)), false);
  assert.equal(q.size, 0);
});

test("ChunkQueue: init first, then media in FIFO order", () => {
  const q = new ChunkQueue();
  assert.equal(q.push(init), true);
  assert.equal(q.push(media(1)), true);
  assert.equal(q.push(media(2)), true);
  assert.equal(q.size, 3);
  assert.deepEqual([...q.shift()], [1]);
  assert.deepEqual([...q.shift()], [1]);
  assert.deepEqual([...q.shift()], [2]);
  assert.equal(q.shift(), undefined);
});

test("ChunkQueue: a new init discards pending media from the old generation", () => {
  const q = new ChunkQueue();
  q.push(init);
  q.push(media(1));
  q.push(media(2));
  q.push({ initSegment: true, payload: new Uint8Array([9]) });
  assert.equal(q.size, 1);
  assert.deepEqual([...q.shift()], [9]);
});

test("ChunkQueue: reset clears everything including init state", () => {
  const q = new ChunkQueue();
  q.push(init);
  q.push(media(1));
  q.reset();
  assert.equal(q.size, 0);
  // After reset, media is dropped again until the next init.
  assert.equal(q.push(media(1)), false);
});
