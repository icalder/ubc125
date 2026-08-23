# De-clicker — new approach

Working plan for the de-clicker being developed on the `declick-next`
branch (off `main`). The wavelet approach is abandoned; its full history
(algorithm, config-gated improvements, A/B harness) is preserved on the
`declick` branch and is not carried forward.

## Decisions

- **Ditch the wavelet de-clicker.** Click-vs-speech classification per
  transient did not generalize: speech onsets (plosives, trills) are
  shape-identical to clicks, and every discriminator tried (shape,
  tail, silence contrast) traded voice quality against click
  cleanliness.
- **Tuning ground truth is raw ALSA PCM** captured on the Pi (`hw:2`,
  48 kHz mono S16_LE) and streamed to the dev machine over an SSH pipe —
  **not** the Opus-decoded `Listen` stream: the filter runs *before*
  Opus encoding, so it must be tuned against the signal it actually
  sees. Capture method: [test-audio/README.md](./test-audio/README.md).
  Current capture: `test-audio/raw60.wav` (60 s of scanning: channel-
  switch clicks, speech, long squelch-on gaps).
- **Seam first, null filter second.** The filter insertion point is in
  place and regression-tested byte-identical; the only filter that
  exists is `PassThrough` (null). `serve` is unchanged — no flag, no
  filter wired in.

## Branch state (`declick-next`)

| Commit | Content |
|---|---|
| `docs: raw PCM capture method for de-clicker tuning` | AGENTS.md paragraph + new `test-audio/README.md` (SSH-pipe `arecord` capture, WAV wrap). |
| `audio: PCM filter seam ... (null filter only)` | `src/audio/filter.rs` (`PcmFrameFilter` + `PassThrough`); wiring in `src/audio/native.rs` (`with_filter` on both sources, per-generation `for_capture`, per-frame `process_frame` before Opus); seam regression test. |

## Established facts (from `raw60.wav`)

- **The scanner's squelch state is detectable from the audio level.**
  Squelch on (no signal): 200 ms block peaks −56…−59.7 dBFS,
  RMS −65…−73 dBFS. Active signal: ≥ −37 dBFS in steady state. A
  ~−50 dBFS threshold on a 50–100 ms peak window separates the two
  classes with ~10 dB margin.
- Squelch-close **fades** cross that band for ~200–400 ms → the state
  detector needs hysteresis (or a longer confirmation window).
- Channel-switch clicks occur only in voice-free (squelch-on) gaps —
  the scanner only switches channels while scanning. Speech onsets are
  shape-identical to clicks (fast attack), which is why per-transient
  classification failed.

## Working direction

Use the squelch state as a coarse gate instead of classifying every
transient:

- **Squelch on** (no transmission): the line carries background noise
  plus any channel-switch clicks → de-click here.
- **Squelch off** (transmission): speech is present → do not touch the
  audio.

This removes speech from the de-clicker's problem: speech can only
exist squelch-off, where nothing is processed.

### Open questions

1. **Squelch detector design:** window length, threshold, hysteresis /
   confirmation, and the latency it adds. Must run in the 20 ms-frame
   streaming filter.
2. **What de-clicking does in the squelch-on state:** click detection +
   replacement is still needed (or something coarser — e.g. a noise-
   gate-like treatment — if the squelch-on content is "just noise +
   clicks" enough that any removal is acceptable).
3. **Harness:** the wavelet A/B harness lives on the `declick` branch;
   the new approach needs its own raw-PCM in/out harness (input
   `raw60.wav`, output WAV, A/B by ear + metrics).

## Next steps

1. Offline squelch state detector against `raw60.wav`: label the level
   timeline, check transitions against the audible fades, pick window /
   threshold / hysteresis.
2. Decide the squelch-on treatment (open question 2).
3. Streaming `PcmFrameFilter` implementation on the seam; A/B harness;
   user listens.
