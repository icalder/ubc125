# De-clicker — current plan

> **Superseded.** The squelch-gate plan in this document is superseded by
> [ML-PORT-PLAN.md](./ML-PORT-PLAN.md): `src/audio/squelch.rs` and
> `examples/squelch_gate.rs` are removed, and `--declick` now enables the
> plateau-trigger de-clicker ported 1:1 from `../ubc125-ml/src/clickfilter/`
> (T3 record config, fixed 20.5 ms delay, off by default). The sections below
> are kept as the history of the squelch-gate workstream.

Working plan for the de-clicker on the `declick-next` branch. The wavelet
approach is abandoned; its history is preserved on the `declick` branch and
is not carried forward.

## Context

The audio is voice and speech from a UBC125XLT radio scanner, captured from
its mic line. The target events are the squelch open and close transients at
the start and end of transmissions. In-transmission speech must remain
untouched.

## Agreed facts

1. An RF transmission ends when the user releases the microphone, or when
   the received signal falls below the scanner's squelch threshold.
2. Releasing the microphone can cause a level-change transient in the
   scanner's audio path. This transient is heard as a sharp, loud closing
   click in the raw PCM. A DC-coupling mechanism is plausible, but is not
   yet proven.
3. After the click, the scanner closes its squelch. The transmission audio
   disappears and the input falls through a residual settling tail to the
   system noise floor. The fall is not always instantaneous.
4. Let `T` be the time at which the squelch floor is confirmed. The intended
   generic off-fade ends at `T` and starts `n` samples earlier:

   ```text
   fade_start = T - n samples
   fade_end   = T
   ```

   At 48 kHz, 20 ms is 960 samples.
5. Increase `n` during listening tests until the clicks are suppressed or
   the fade begins to affect the final speech. This is an A/B method, not a
   production value by itself.
6. `test-audio/raw60.wav` is a representative capture, not a timing
   template. Do not tune a curve to the position of one click in this file.
   The final behavior must be checked against more captures and multiple
   transmission endings.

## Where we are now

The squelch gate is implemented and tested in `src/audio/squelch.rs`. The
offline harness is `examples/squelch_gate.rs`. It is now available in
`serve` behind the experimental `--declick` flag. The production interim
configuration uses a 20 ms onset fade and a 1000 ms floor-anchored close
fade. Without `--declick`, `PassThrough` remains the production filter.

The on-side works: a sample-anchored 20 ms fade-in suppresses the release
clicks at transmission starts without damaging the following speech.

The off-side now works mechanically as a delayed, floor-anchored fade, but
the default 20 ms fade does not reduce the closing clicks. Increasing the
fade duration eventually reduces the click, but also attenuates preceding
speech. This is now understood as a timing and curve trade-off, not as the
original zero-delay bug.

The current A/B observations from `raw60.wav` are:

| Off-fade | Result around the approximately 36.46 s full-scale click |
|---:|---|
| 20 ms | No audible improvement |
| 40 ms | No audible improvement |
| 100 ms | No audible improvement |
| 400 ms | No audible improvement; the click is still effectively full level |
| 500 ms | Approximately 0.7 dB peak reduction; not audible as suppression |
| 1000 ms | Approximately 8.7 dB peak reduction |
| 1500 ms | Approximately 15 dB peak reduction; user reports that it works, but speech is attenuated |
| 2000 ms | Approximately 20 dB peak reduction |

These values are measurements from one event and are not locked settings.
The 1500 ms result proves that the click is reachable by a floor-anchored
fade, but it does not prove that 1500 ms or the current curve is suitable.

## Current implementation

The PCM seam remains in-place:

```rust
PcmFrameFilter::process_frame(&mut self, frame: &mut [i16])
```

`SquelchGate` keeps an internal frame look-back buffer. Each buffered frame
keeps its original samples and a separate per-sample gain schedule. This
allows a later floor confirmation to edit PCM that has not yet reached the
encoder.

Configuration defaults and rules:

```text
close_db          = -45.0 dBFS
reopen_db         = -42.0 dBFS
close_confirm_ms  = 20
fade_ms           = 20       # fade-in and ramp-back
fade_out_ms       = 0        # inherit fade_ms; never shorter than fade_ms
delay_frames      = 2        # 40 ms minimum look-back by default
```

The close detector counts consecutive samples below `close_db`. The sample
that completes the confirmation run is the fade endpoint. The close fade
starts `fade_out_ms` earlier and is applied retroactively to the buffered
frames. A configured close fade longer than the configured delay causes the
gate to increase its actual buffer latency automatically. A delay of zero is
an explicit zero-latency control and does not provide retroactive look-back.

The `PcmFrameFilter` trait has a `flush()` method so finite sources can emit
frames still held by a delayed filter. `PassThrough` keeps the default empty
flush and remains byte-identical to the no-filter path.

## Interim production rollout

`serve --declick` constructs the native ALSA source with:

```text
fade_ms       = 20 ms
fade_out_ms   = 1000 ms
close_db      = -45 dBFS
reopen_db     = -42 dBFS
close_confirm = 20 ms
```

The gate automatically retains enough PCM frames for the 1000 ms look-back.
The `UBC125_DECLICK=1` environment variable enables the same mode. The
`UBC125_AUDIO_CMD` test hook is already a WebM source and is not filtered.

This is an interim rollout, not a locked de-clicker configuration. It is
intended to provide useful closing-click reduction while further captures
and listening tests determine whether the speech attenuation is acceptable.

The harness supports:

```text
--close DB
--reopen DB
--confirm MS
--fade MS
--fade-out MS
--delay FRAMES
```

Offline output removes the startup look-back latency and keeps the WAV
aligned with the input. The files used for the current listening round
include:

```text
test-audio/raw60.wav
test-audio/raw60_gated_delay2.wav
test-audio/raw60_gated_fadeout40.wav
test-audio/raw60_gated_fadeout100.wav
test-audio/raw60_gated_fadeout400.wav
test-audio/raw60_gated_fadeout500.wav
test-audio/raw60_gated_fadeout1000.wav
test-audio/raw60_gated_fadeout1500.wav
test-audio/raw60_gated_fadeout2000.wav
```

## Why the two sides behave differently

Fade-in has a useful asymmetry: the gate can identify the first loud sample
at the onset, buffer it, and apply the fade directly to the release click
before emitting it.

At an offset, the closing click is itself an upward spike. While it occurs,
the level detector still sees an open transmission and does not close the
gate. The gate learns that the transmission has ended only after the click,
when the signal has settled below the close threshold for the confirmation
interval.

A short fade ending at that later time only attenuates the settling tail. It
cannot attenuate the earlier click. A 500 ms raised-cosine fade still leaves
most of the click near full gain because the click occurs near the beginning
of that curve. The 1500 ms fade attenuates it, but begins affecting the
preceding speech.

This exposes a conflict in the earlier plan:

- A generic floor-anchored fade can be made long enough to suppress the
  closing click.
- A short fade preserves speech but cannot reach an earlier closing click.
- A fixed curve cannot remove this trade-off for arbitrary settling times.

This is not fixed by adding more frame buffering alone. Buffering makes the
chosen look-back editable; it does not decide which earlier samples should
be attenuated.

## Current decisions and decisions still open

### Kept

- Use raw ALSA PCM as the tuning ground truth, not Opus-decoded audio.
- Keep the level-driven squelch gate for noise-floor muting and onset fade-in.
- Keep the fade endpoint at the confirmed noise floor.
- Keep the internal delayed buffer rather than changing the public seam to
  an input/output pair.
- Keep `PassThrough` as the default production path until the off behavior
  is accepted.
- Do not optimize the curve for one timestamp or one capture.

### Open

The original decision to forbid all click-specific handling is now in
question. It is compatible with the generic floor fade, but the generic
fade requires a long look-back and can attenuate speech. We must choose
between:

1. Accepting and tuning a generic long fade.
2. Finding a curve that gives an acceptable robust trade-off across captures.
3. Adding a separate end-transient signal or detector, despite the earlier
   decision against special cases.

No value of `fade_out_ms` is locked yet.

## Proposals

### Proposal A — evaluate curve families without fitting one capture

Keep the floor endpoint and test curve families with the same durations
across every available ending:

1. The current raised cosine as the baseline.
2. A power-shaped raised cosine, which moves attenuation earlier or later in
   the fade in a controlled way.
3. A monotonic knee curve that stays near unity for part of the fade and
   falls more sharply near the endpoint.

The curve must always be monotonic from 1 to 0 and must reach zero at `T`.
A curve parameter must be explicit in `SquelchGateConfig` and the harness,
not encoded from the timestamp of the 36.46 s event.

Expected result: this may reduce speech damage, but it cannot eliminate the
fundamental timing trade-off when click position varies relative to `T`.

### Proposal B — use a robust long generic fade only if new captures support it

Collect several raw captures with different transmission strengths and
settling times. Test a small duration grid, for example 500, 1000, 1500,
and 2000 ms, using the same curve for all captures. Listen to:

- The closing click.
- The last 1–2 seconds of speech before the floor.
- Clean endings without a click.
- Pauses and short transmissions.
- Onsets after each gap.

Choose the shortest duration that gives acceptable click reduction across
captures without unacceptable speech attenuation. Do not select a duration
from `raw60.wav` alone.

### Proposal C — separate the closing-click problem from the generic gate

If Proposal B damages speech, keep the generic gate for floor muting and
onset fades, but use a separate source of end-of-transmission information.
Possible sources are:

- Scanner squelch/status information, if it can be obtained with suitable
  timing.
- A dedicated end-transient detector that identifies an abrupt closing
  event in buffered PCM.
- A transmission-end envelope detector with a short retroactive fade.

This would be a deliberate change to the earlier “no click detection”
decision. It should only be proposed after the multi-capture A/B tests show
that a generic fade cannot meet the speech-preservation requirement.

## Validation requirements for the interim rollout

1. Keep the 1000 ms setting available behind `--declick`; do not make it the
   default audio path yet.
2. Test synthetic frame boundaries, endpoint timing, cancellation, and
   buffer flushing.
3. Run the full test suite.
4. Validate against multiple raw PCM captures, not only `raw60.wav`.
5. Listen to complete files, not only numerical peak reports.
6. Confirm the on-fade remains approximately 20 ms and does not alter
   steady-state speech.
7. Confirm `PassThrough` remains byte-identical.
8. Record user feedback on click reduction and speech attenuation before
   selecting the next curve or duration.
9. After the generic approach is accepted or rejected, update this plan and
   decide whether to retain the flag, change its configuration, or replace
   it with a separate end-transient approach.
