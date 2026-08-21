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
    const head = sb.buffered.start(0);
    const tail = sb.buffered.end(0);
    const removeHeadEnd = Math.max(head, audio.currentTime - 3);
    if (removeHeadEnd > head) {
      sb.remove(head, Math.min(removeHeadEnd, tail));
      return true; // one range per pass
    }
    const tailLimit = audio.currentTime + 10;
    if (tail > tailLimit) {
      sb.remove(Math.max(head, tailLimit - 3), tail);
      return true;
    }
    return false;
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
