// Audio stream helper for AudioService.Listen.
//
// Plays the server's segmented WebM/Opus byte stream (init segment, then
// cluster chunks) through a MediaSource. The pure logic (state machine,
// chunk queue) is unit-tested in tests/audio.test.js; AudioStream is the
// thin browser glue around it. No worker, no new dependencies.

import { INITIAL_BACKOFF, MAX_BACKOFF, nextBackoff } from "./backoff.js";

/** MIME type of the segmented WebM/Opus stream the server sends. */
export const AUDIO_MIME = 'audio/webm; codecs="opus"';

/** All audio states, in display order. */
export const AUDIO_STATES = [
  "off",
  "connecting",
  "playing",
  "reconnecting",
  "unavailable",
];

/**
 * State machine transitions (pure).
 *
 * Events:
 *  - "play":        user pressed Play (only from off).
 *  - "unsupported": browser cannot play WebM/Opus (on play, from off).
 *  - "ready":       stream is flowing and the init segment was appended.
 *  - "error":       stream failed or ended; the helper reconnects.
 *  - "stop":        user pressed Stop (from any active state).
 *
 * Unknown events are no-ops; the current state is returned.
 */
export function audioTransition(state, event) {
  switch (state) {
    case "off":
      switch (event) {
        case "play":
          return "connecting";
        case "unsupported":
          return "unavailable";
      }
      return state;
    case "connecting":
      switch (event) {
        case "ready":
          return "playing";
        case "error":
          return "reconnecting";
        case "stop":
          return "off";
      }
      return state;
    case "playing":
      switch (event) {
        case "error":
          return "reconnecting";
        case "stop":
          return "off";
      }
      return state;
    case "reconnecting":
      switch (event) {
        case "ready":
          return "playing";
        case "stop":
          return "off";
      }
      return state;
    case "unavailable":
      if (event === "stop") return "off";
      return state;
  }
  return state;
}

/**
 * The playhead position a late joiner must seek to, or null when no seek
 * is needed.
 *
 * A client that joins a running generation receives clusters whose
 * timecodes begin at the generation's elapsed time; its fresh playhead
 * sits at 0 with nothing buffered there and would stall in silence
 * forever while the UI shows "playing". Seeking to the earliest buffered
 * data starts playback at the join point. A join at the head of the
 * generation (buffer from 0) needs no seek.
 *
 * @param {number} bufferedStart the start (s) of the first buffered range
 * @returns {number|null} the seek target, or null
 */
export function lateJoinSeek(bufferedStart) {
  // 1 ms epsilon: a head join's first cluster has timecode 0; anything
  // past the epsilon means we joined mid-generation.
  return bufferedStart > 0.001 ? bufferedStart : null;
}

/**
 * Decide whether a generation was joined at its head or mid-stream.
 *
 * The decision is made exactly once, from the FIRST non-empty buffered
 * state of the generation, and is sticky. That matters because the
 * buffer head-trim (which keeps ~3 s behind the playhead) moves
 * `buffered.start(0)` forward on a head join seconds later; judging the
 * join kind from a later observation would misread the trimmed head as a
 * late join and seek the playhead back into already-played audio.
 *
 * @param {string|null} decision prior decision: "head", "late", or null
 * @param {number|null} bufferedStart start of the first buffered range, or null when the buffer is still empty
 * @returns {string|null} "head", "late", or null (still undecided)
 */
export function decideJoinKind(decision, bufferedStart) {
  if (decision !== null) return decision;
  if (bufferedStart === null) return null;
  return bufferedStart > 0.001 ? "late" : "head";
}

/**
 * How close to the end of a buffered range a playhead counts as stalled (s).
 *
 * A live append extends the buffer continuously, so a playhead within this
 * distance of a range end and with a later range behind it is not waiting for
 * data — MSE has stopped it at a gap. Short enough that the skip costs little
 * audio, long enough that a normal append in flight is not skipped over.
 */
export const GAP_STALL_S = 0.12;

/** Audio kept behind the playhead by the SourceBuffer head trim (s). */
export const TRIM_HEAD_KEEP_S = 3;

/** How far ahead of the playhead the buffered tail may run (s). */
export const TRIM_TAIL_CAP_S = 10;

/** Landing point of a live-edge catch-up: this far before the buffered tail (s). */
export const LIVE_KEEP_AHEAD_S = 1.5;

/**
 * The forward seek a stalled playhead needs (B8: skip, never wait).
 *
 * Dropping whole chunks is normal policy (B5: bounded queues, drop-oldest),
 * so a hole in the buffered timeline is normal too. MSE stops the playhead at
 * the end of a buffered range and waits, which is a permanent freeze — the
 * label keeps saying "playing" while the audio is gone. Two shapes of that
 * freeze, and both seek forward:
 *   - the playhead sits at the tail of a range and a later range exists →
 *     seek to that range's start;
 *   - the playhead sits in a hole (or behind the buffer head: a late join, or
 *     a head the trim has discarded) → seek to the next buffered sample.
 * Without the second case a playhead left between two ranges stays there
 * forever — measured 19.8 s of silence under a "playing" label once the tail
 * cap actually fired.
 *
 * @param {object} state
 * @param {Array<[number,number]>} ranges the SourceBuffer's buffered ranges, in order
 * @param {number} currentTime the playhead position (s)
 * @param {number} [stallS] how close to a range end counts as stalled
 * @returns {number|null} the seek target, or null when there is nothing to skip to
 */
export function gapSkip({ ranges, currentTime, stallS = GAP_STALL_S }) {
  for (const [i, range] of ranges.entries()) {
    const [start, end] = range;
    if (currentTime < start) {
      // The playhead sits in a hole — between two ranges, or behind the
      // buffer head (a late join, or a head that the trim has already
      // discarded). Resume at the next buffered sample.
      return start > currentTime + 0.001 ? start : null;
    }
    if (currentTime <= end) {
      // Inside this range; only its tail matters. Away from the tail an
      // append in flight would have extended it — not a stall.
      if (currentTime < end - stallS) return null;
      const next = ranges[i + 1];
      // No later range means the live edge: waiting there is correct.
      return next && next[0] > currentTime + 0.001 ? next[0] : null;
    }
    // Past this range's end: the next iteration decides (hole or live edge).
  }
  return null;
}

/** Minimum milliseconds between live-edge catch-up seeks. */
export const CATCHUP_MIN_INTERVAL_MS = 500;

/**
 * Where to seek when the buffered tail has run far ahead of the playhead.
 *
 * Removing a far-ahead tail is not a fix on its own: a producer faster than
 * real time — the `audio-tone --loop` test fixture runs ~250x — appends past
 * the removed window, so the playhead starves at the hole while the label
 * still says "playing" (measured: silent for the rest of a 24 s run, the
 * SourceBuffer gone). Bounding held audio and keeping the playhead near live
 * are the same requirement, so the client jumps to the live edge instead,
 * and the ranges it passes are then behind the playhead and trimmed. Against
 * the scanner (append rate ~1x) the tail is ~1 s ahead and this never fires.
 *
 * @param {object} state
 * @param {Array<[number,number]>} ranges the buffered ranges, in order
 * @param {number} currentTime the playhead position (s)
 * @param {number} [tailCapS] how far ahead of the playhead the tail may run
 * @param {number} [keepAheadS] landing distance before the buffered tail
 * @returns {number|null} the seek target, or null when the playhead is in range
 */
export function liveEdgeSeek({
  ranges,
  currentTime,
  tailCapS = TRIM_TAIL_CAP_S,
  keepAheadS = LIVE_KEEP_AHEAD_S,
}) {
  if (ranges.length === 0) return null;
  const [lastStart, tail] = ranges[ranges.length - 1];
  if (tail - currentTime <= tailCapS) return null;
  const target = Math.max(lastStart, tail - keepAheadS);
  return target > currentTime + 0.001 ? target : null;
}

/**
 * The next SourceBuffer window to remove (B7's mechanism, extracted so the
 * math is testable): what is far behind the playhead.
 *
 * Nothing ahead of the playhead is removed here. A cap on the ahead side
 * (the buffer's last range > playhead + TRIM_TAIL_CAP_S) was tried and
 * starves the playhead whenever the producer runs faster than real time — see
 * `liveEdgeSeek`, which bounds the same window by moving the playhead. Held
 * audio stays bounded because everything the catch-up jumps past becomes
 * behind-side audio and is trimmed on the next pass.
 *
 * @param {object} state
 * @param {Array<[number,number]>} ranges the SourceBuffer's buffered ranges, in order
 * @param {number} currentTime the playhead position (s)
 * @param {number} [headKeepS] audio kept behind the playhead
 * @returns {{from:number,to:number}|null} the window to remove, or null
 */
export function trimPlan({
  ranges,
  currentTime,
  headKeepS = TRIM_HEAD_KEEP_S,
}) {
  if (ranges.length === 0) return null;
  const head = ranges[0][0];
  const tail = ranges[ranges.length - 1][1];
  const removeHeadEnd = currentTime - headKeepS;
  if (removeHeadEnd > head) {
    return { from: head, to: Math.min(removeHeadEnd, tail) };
  }
  return null;
}

/**
 * FIFO of chunks waiting to be appended to the SourceBuffer.
 *
 * Guarantees no media is ever queued before an init segment has been seen
 * (the server sends exactly one init per generation, always first). An
 * init also discards pending media from the previous generation, which
 * belonged to a different MediaSource.
 */
export class ChunkQueue {
  constructor() {
    this._items = [];
    this._sawInit = false;
  }

  /** Push a chunk (`{ initSegment, payload }`). True if it was kept. */
  push(chunk) {
    if (chunk.initSegment) {
      this._items.length = 0;
      this._sawInit = true;
      this._items.push(chunk.payload);
      return true;
    }
    if (!this._sawInit) return false;
    this._items.push(chunk.payload);
    return true;
  }

  /** Take the oldest queued payload, or undefined when empty. */
  shift() {
    return this._items.shift();
  }

  get size() {
    return this._items.length;
  }

  /** Discard everything (used on stop / generation reset). */
  reset() {
    this._items.length = 0;
    this._sawInit = false;
  }
}

// -- browser glue ------------------------------------------------------------

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Resolve on the next `event` of `el` (one-shot). */
function once(el, event) {
  return new Promise((resolve) =>
    el.addEventListener(event, resolve, { once: true })
  );
}

/**
 * The SourceBuffer's buffered ranges as plain [start, end] pairs (s), so
 * gapSkip/trimPlan are pure functions unit-testable without a browser.
 *
 * @param {SourceBuffer|null} sb
 * @returns {Array<[number,number]>}
 */
function bufferedRanges(sb) {
  const ranges = [];
  if (!sb) return ranges;
  for (let i = 0; i < sb.buffered.length; i++) {
    ranges.push([sb.buffered.start(i), sb.buffered.end(i)]);
  }
  return ranges;
}

/**
 * Plays the AudioService.Listen stream into a detached `<audio>` element.
 *
 * States: off -> connecting -> playing, with "reconnecting" on stream
 * failure (bounded exponential backoff, reusing lib/backoff.js) and
 * "unavailable" when the browser cannot decode the stream.
 */
export class AudioStream {
  /**
   * @param {object} client generated AudioService connect client.
   * @param {object} [opts]
   * @param {(state: string) => void} [opts.onState] state change callback.
   */
  constructor(client, { onState } = {}) {
    this._client = client;
    this._onState = onState;
    this.state = "off";
    this._queue = new ChunkQueue();
    this._stopped = true;
    this._backoff = INITIAL_BACKOFF;
    this._audio = null;
    this._ms = null;
    this._msUrl = null; // object URL of the current MediaSource
    this._sb = null;
    this._sbReady = null; // Promise: current SourceBuffer is open
    this._joinKind = null; // decided once per generation ("head"|"late")
    this._seeked = false; // late-join seek issued for this generation
    this._wake = null; // waiter for the next queued chunk
    this._run = 0; // run id; stale loops observe a mismatch and exit
    this._lastTrim = 0;
    this._gapSkips = 0; // B8 seeks over dropped-chunk gaps (this run)
    this._lastCatchup = 0;
    this._catchups = 0; // live-edge jumps (this run): audio deliberately lost
  }

  /** Can this browser play WebM/Opus through MediaSource? */
  static isSupported() {
    return (
      typeof MediaSource !== "undefined" &&
      MediaSource.isTypeSupported(AUDIO_MIME)
    );
  }

  _setState(next) {
    if (next === this.state) return;
    this.state = next;
    if (this._onState) this._onState(next);
  }

  _transition(event) {
    this._setState(audioTransition(this.state, event));
  }

  /** Start capture + playback (user gesture; idempotent while active). */
  async play() {
    if (this.state !== "off") return;
    if (!AudioStream.isSupported()) {
      this._transition("unsupported");
      return;
    }
    this._stopped = false;
    this._backoff = INITIAL_BACKOFF;
    // Create and start the element synchronously inside the user gesture
    // so the autoplay policy allows playback.
    const audio = new Audio();
    this._audio = audio;
    const run = ++this._run;
    this._setState("connecting");
    // One drain loop per run; it exits on stop (run mismatch + wake).
    this._drainLoop(run);
    await this._loop(run);
  }

  /** Stop playback and release everything (explicit only). */
  stop() {
    if (this.state === "off") return;
    this._stopped = true;
    this._run++;
    this._queue.reset();
    this._teardownAudio();
    this._setState("off");
    // An aborted fetch keeps the keep-alive TCP connection open, so the
    // server never notices the client is gone; ask it to stop the capture
    // explicitly (releases the mic on the Pi). Fire-and-forget: stop must
    // not hang on the network.
    this._client
      .stopCapture({})
      .catch(() => {});
  }

  /** Subscribe loop: reconnect with bounded backoff until stopped. */
  async _loop(run) {
    for (;;) {
      if (this._stopped || run !== this._run) return;
      try {
        for await (const chunk of this._client.listen({})) {
          if (this._stopped || run !== this._run) return;
          this._queue.push(chunk);
          if (chunk.initSegment) this._beginGeneration();
          this._wakeNow();
        }
        throw new Error("audio stream ended");
      } catch (e) {
        if (this._stopped || run !== this._run) return;
        this._discardGeneration();
        this._queue.reset();
        this._transition("error");
        const delay = this._backoff;
        this._backoff = nextBackoff(this._backoff, MAX_BACKOFF);
        await sleep(delay);
      }
    }
  }

  /** Append queued payloads to the SourceBuffer until stopped. */
  async _drainLoop(run) {
    for (;;) {
      if (this._stopped || run !== this._run) return;
      const ready = this._sbReady;
      if (ready) {
        await ready.promise;
        if (this._stopped || run !== this._run) return;
        if (this._sbReady !== ready) continue; // generation changed
        for (;;) {
          if (this._stopped || run !== this._run) return;
          const sb = this._sb;
          if (!sb) break; // generation discarded; re-check outer state
          // A playhead MSE has stopped at a gap must not wait for audio that
          // is never coming; seeking is not a SourceBuffer mutation, so this
          // runs even while an update is in flight.
          if (this._catchupIfNeeded()) continue;
          if (this._gapSkipIfNeeded()) continue;
          if (sb.updating) {
            await once(sb, "updateend");
            continue;
          }
          // A late joiner's buffer starts at the generation's elapsed
          // time; seek the playhead to the earliest available data.
          if (this._seekIfNeeded()) continue;
          // A trim issues a remove(); the next pass must wait for its
          // updateend before appending (concurrent mutation throws).
          if (this._trimIfNeeded()) continue;
          const data = this._queue.shift();
          if (data) {
            try {
              sb.appendBuffer(data);
            } catch {
              // MSE rejected the append; drop the generation. The next
              // init segment starts a fresh one.
              this._discardGeneration();
              this._queue.reset();
              continue;
            }
            if (
              this.state === "connecting" ||
              this.state === "reconnecting"
            ) {
              this._transition("ready");
            }
          } else {
            await this._nextChunk();
          }
        }
      } else {
        // No generation yet: wait for the init chunk to open one.
        await this._nextChunk();
      }
    }
  }

  /**
   * Seek a late joiner's playhead to the earliest buffered data (see
   * lateJoinSeek). At most once per generation, and only after the first
   * append has been committed (the buffer is non-empty and not updating).
   *
   * @returns {boolean} true when a seek was issued.
   */
  _seekIfNeeded() {
    if (this._seeked || this._joinKind === "head") return false;
    const sb = this._sb;
    const audio = this._audio;
    if (!sb || !audio || sb.updating || sb.buffered.length === 0) return false;
    // The first buffered data decides the join kind; head-trims later move
    // buffered.start(0) without changing it (see decideJoinKind).
    this._joinKind = decideJoinKind(this._joinKind, sb.buffered.start(0));
    if (this._joinKind !== "late") return false;
    const target = lateJoinSeek(sb.buffered.start(0));
    this._seeked = true;
    audio.currentTime = target;
    return true;
  }

  /**
   * Jump a playhead that has fallen far behind the buffered tail (see
   * `liveEdgeSeek`). Cadence-limited: against the faster-than-real-time test
   * fixture the tail runs ahead again within a second, and a seek per second
   * is enough to keep the client near live without thrashing.
   *
   * @returns {boolean} true when a seek was issued.
   */
  _catchupIfNeeded() {
    const sb = this._sb;
    const audio = this._audio;
    if (!sb || !audio || audio.paused) return false;
    const now = Date.now();
    if (now - this._lastCatchup < CATCHUP_MIN_INTERVAL_MS) return false;
    const target = liveEdgeSeek({
      ranges: bufferedRanges(sb),
      currentTime: audio.currentTime,
    });
    if (target === null) return false;
    this._lastCatchup = now;
    this._catchups++;
    audio.currentTime = target;
    return true;
  }

  /**
   * Seek a playhead that MSE has stopped at a gap (see `gapSkip`). Not
   * cadence-limited: MSE stops the playhead dead at a range end, so waiting
   * costs silence, and a repeat seek to the same target is impossible (the
   * target is ahead of the playhead and the playhead moves after it).
   *
   * @returns {boolean} true when a seek was issued.
   */
  _gapSkipIfNeeded() {
    const sb = this._sb;
    const audio = this._audio;
    if (!sb || !audio || audio.paused) return false;
    const target = gapSkip({
      ranges: bufferedRanges(sb),
      currentTime: audio.currentTime,
    });
    if (target === null) return false;
    this._gapSkips++;
    audio.currentTime = target;
    return true;
  }

  /** Start a fresh MediaSource for a new init segment (generation). */
  _beginGeneration() {
    this._discardGeneration();
    this._joinKind = null; // a fresh generation gets a fresh decision
    this._seeked = false; // the new generation starts at timecode 0
    const ms = new MediaSource();
    this._ms = ms;
    const audio = this._audio;
    // Chromium does not accept a main-thread MediaSource via srcObject
    // (only MediaStream / worker MediaSourceHandle); the object-URL form
    // is the supported attachment.
    this._msUrl = URL.createObjectURL(ms);
    audio.src = this._msUrl;
    audio.play().catch(() => {}); // gesture allowance from play()
    let resolveOpen;
    const promise = new Promise((resolve) => {
      resolveOpen = resolve;
    });
    ms.addEventListener("sourceopen", () => {
      if (this._ms !== ms) return; // discarded before it opened
      resolveOpen();
      this._sb = ms.addSourceBuffer(AUDIO_MIME);
      this._wakeNow();
    }, { once: true });
    // { promise, resolve }: discard resolves a stale promise so a drain
    // loop waiting on a dead generation is never stranded.
    this._sbReady = { promise, resolve: resolveOpen };
  }

  /** Discard the current MediaSource/SourceBuffer (if any). */
  _discardGeneration() {
    const ms = this._ms;
    const url = this._msUrl;
    const ready = this._sbReady;
    this._ms = null;
    this._sb = null;
    this._sbReady = null;
    this._msUrl = null;
    if (ready) ready.resolve();
    if (ms && ms.readyState !== "closed") {
      try {
        ms.endOfStream("abort");
      } catch {
        // Already closing; nothing to release.
      }
    }
    if (url) URL.revokeObjectURL(url);
  }

  /** Pause and detach the audio element. */
  _teardownAudio() {
    this._discardGeneration();
    const audio = this._audio;
    this._audio = null;
    this._wakeNow();
    if (audio) {
      audio.pause();
      audio.removeAttribute("src");
    }
  }

  /**
   * Keep the buffer near the playhead: drop the old head (~3 s behind) and,
   * for faster-than-real-time sources, cap the future tail (~10 s ahead) so
   * the SourceBuffer cannot bloat and stall the main thread.
   *
   * @returns {boolean} true when a remove() was issued (the caller must
   *   wait for updateend before appending).
   */
  _trimIfNeeded() {
    const sb = this._sb;
    const audio = this._audio;
    if (!sb || !audio || sb.updating || sb.buffered.length === 0) return false;
    if (performance.now() - this._lastTrim < 2000) return false;
    this._lastTrim = performance.now();
    const plan = trimPlan({
      ranges: bufferedRanges(sb),
      currentTime: audio.currentTime,
    });
    if (!plan) return false;
    sb.remove(plan.from, plan.to); // one window per pass
    return true;
  }

  /** Wait until a chunk is queued (or the run is torn down). */
  _nextChunk() {
    return new Promise((resolve) => {
      this._wake = resolve;
    });
  }

  _wakeNow() {
    const wake = this._wake;
    this._wake = null;
    if (wake) wake();
  }
}
