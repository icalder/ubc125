import { test } from "node:test";
import assert from "node:assert/strict";
import {
  AUDIO_MIME,
  AUDIO_STATES,
  GAP_STALL_S,
  TRIM_HEAD_KEEP_S,
  LIVE_KEEP_AHEAD_S,
  audioTransition,
  gapSkip,
  lateJoinSeek,
  liveEdgeSeek,
  decideJoinKind,
  trimPlan,
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

// -- B8 gap skip -------------------------------------------------------------

test("gapSkip: a playhead inside a range is not stalled", () => {
  assert.equal(gapSkip({ ranges: [[0, 10]], currentTime: 5 }), null);
  assert.equal(gapSkip({ ranges: [], currentTime: 0 }), null);
});

test("gapSkip: a playhead at a range tail seeks to the next range", () => {
  const ranges = [
    [0, 10],
    [12, 20],
  ];
  // 0.02 s from the end: MSE is about to stop the playhead at the gap.
  assert.equal(gapSkip({ ranges, currentTime: 9.98 }), 12);
  // Exactly the stall distance counts as stalled (>= boundary, T5).
  assert.equal(gapSkip({ ranges, currentTime: 10 - GAP_STALL_S }), 12);
  // Just outside it does not (an append in flight must not be skipped over).
  assert.equal(gapSkip({ ranges, currentTime: 10 - GAP_STALL_S - 0.001 }), null);
});

test("gapSkip: the live edge has nothing to skip to", () => {
  // At the end of the LAST range the playhead is waiting for the next
  // chunk, which is correct — waiting there is not a stall.
  assert.equal(gapSkip({ ranges: [[0, 10]], currentTime: 9.99 }), null);
  assert.equal(gapSkip({ ranges: [[5, 10], [12, 20]], currentTime: 19.99 }), null);
});

test("gapSkip: a playhead inside a hole resumes at the next range", () => {
  // The trim drops heads and tails, and drop-oldest drops middle chunks, so
  // the playhead can sit between ranges. Restricted to "only inside a range"
  // (the first version) that state never recovered: 19.8 s of silence under a
  // "playing" label once the tail cap actually fired.
  assert.equal(
    gapSkip({
      ranges: [
        [6.3, 15],
        [2000, 2001],
      ],
      currentTime: 15.4,
    }),
    2000,
  );
});

test("gapSkip: a playhead behind the buffer head jumps to the head", () => {
  // A late join, or a head the trim has already removed. lateJoinSeek does
  // the join case once per generation; this keeps working after it.
  assert.equal(
    gapSkip({ ranges: [[6.3, 15], [2000, 2001]], currentTime: 0 }),
    6.3,
  );
});

test("gapSkip: a playhead past everything has nothing buffered to skip to", () => {
  assert.equal(gapSkip({ ranges: [[0, 10], [12, 20]], currentTime: 25 }), null);
});

test("gapSkip: skips walk forward through consecutive gaps", () => {
  const ranges = [
    [0, 10],
    [12, 12.05],
    [30, 40],
  ];
  let t = 9.95;
  const seen = [];
  for (let i = 0; i < 5; i++) {
    const target = gapSkip({ ranges, currentTime: t });
    if (target === null) break;
    seen.push(target);
    t = target;
  }
  assert.deepEqual(seen, [12, 30]);
});

// -- buffer caps -------------------------------------------------------------

test("trimPlan: audio far behind the playhead is removed", () => {
  assert.deepEqual(
    trimPlan({ ranges: [[0, 12]], currentTime: 10 }),
    { from: 0, to: 10 - TRIM_HEAD_KEEP_S },
  );
  // Nothing far behind: no window (yet).
  assert.equal(trimPlan({ ranges: [[0, 12]], currentTime: 2 }), null);
});

test("trimPlan: nothing ahead of the playhead is removed", () => {
  // A cap enforced by remove() starves the playhead whenever the producer
  // runs faster than real time: the removed window is re-appended beyond the
  // playhead, and MSE leaves it stranded (measured: silent for the rest of a
  // 24 s run). The ahead side is bounded by moving the playhead — liveEdgeSeek.
  assert.equal(trimPlan({ ranges: [[9, 40]], currentTime: 10 }), null);
  // The buffered timeline runs 3995 s past the playhead: still no removal.
  assert.equal(trimPlan({ ranges: [[4, 4000]], currentTime: 5 }), null);
});

test("trimPlan: an empty buffer has no window", () => {
  assert.equal(trimPlan({ ranges: [], currentTime: 5 }), null);
});

// -- live-edge catch-up ------------------------------------------------------

test("liveEdgeSeek: a tail inside the cap is left alone", () => {
  assert.equal(
    liveEdgeSeek({ ranges: [[0, 18]], currentTime: 10, tailCapS: 10 }),
    null,
  );
});

test("liveEdgeSeek: a tail beyond the cap is jumped to", () => {
  // Landing short of the very end, so the next chunk still arrives in time.
  assert.equal(
    liveEdgeSeek({
      ranges: [
        [6.3, 15],
        [900, 1900],
      ],
      currentTime: 9.29,
    }),
    1900 - LIVE_KEEP_AHEAD_S,
  );
});

test("liveEdgeSeek: the landing is clamped to the last range's start", () => {
  // A short final range must not be landed *before* it begins (that would
  // re-create the hole the jump exists to close).
  assert.equal(
    liveEdgeSeek({ ranges: [[94, 95]], currentTime: 5 }),
    94,
  );
});

test("liveEdgeSeek: an empty buffer has nowhere to jump", () => {
  assert.equal(liveEdgeSeek({ ranges: [], currentTime: 5 }), null);
});
