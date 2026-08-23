# ML de-clicker — separate workstream

## Purpose

Develop a small machine-learning classifier that recognises scanner audio
clicks directly from raw PCM. This is a possible long-term replacement for
the generic closing fade, which has an unavoidable trade-off between click
reduction and speech attenuation.

The ML work must remain separate from the current interim de-clicker.

## Interim path

Keep the current squelch gate available through:

```sh
ubc125 serve --declick
```

The interim path uses a 20 ms fade-in and a 1000 ms floor-anchored fade-out.
It is useful for live testing, but it is not a locked long-term solution.
The generic gate must wait for the noise floor to confirm that a transmission
ended. A closing click can occur before that confirmation, so a long
look-back fade is needed. The longer fade can attenuate the final speech.

The ML classifier would address this timing problem by recognising the click
at, or shortly after, the click itself.

## Why a separate ML workstream

The opening click is easy for the current gate to suppress because the signal
crosses the reopen level at the click. The closing click is harder: it is an
upward transient that occurs before the scanner closes its squelch and before
the audio reaches the noise floor.

A generic fade anchored at the later floor event can suppress the closing
click only when it is long enough to reach back to the click. This can damage
preceding speech. A classifier can provide a separate decision about whether
a transient is a click or speech, while the squelch gate continues to handle
noise-floor muting.

Do not replace the interim gate until the classifier is validated on unseen
recordings.

## Corpus

Use raw ALSA PCM captured before Opus encoding. Do not use Opus-decoded audio
for training or validation.

The corpus must include recordings from multiple scanner sessions and should
contain labels for:

- Closing clicks.
- Opening or release clicks.
- Channel-switch clicks.
- Speech onsets and plosives.
- Fricatives and other sharp speech sounds.
- Clean transmission endings.
- Noise-floor transitions without clicks.
- Different signal strengths.
- Different post-click settling times.
- Different background noise conditions.

The corpus must contain hard negatives. In particular, speech onsets can
have a shape similar to scanner clicks and must not be treated as clicks only
because they are sharp.

### Label structure

At minimum, label:

```text
class: click / speech / clean-transition / noise
position: sample index of the transient
polarity: if useful
source: recording/session identifier
transmission: identifier within the recording
```

The label should also record the desired correction interval or gain
response if a fixed click replacement envelope is tested.

### Dataset split

Split by complete recording or scanner session, not by random windows. A
random window split can put nearly identical audio from one recording in
both training and validation and produce misleading results.

Reserve at least one complete recording for final evaluation. Do not use the
final evaluation recordings while choosing features, model size, thresholds,
or correction envelopes.

## Initial model proposals

Start with the smallest and most inspectable model that can work.

### Proposal A — linear classifier

Extract compact features from a short PCM window around a candidate
transient, then use logistic regression or another linear classifier.

Candidate features:

- Multi-band energy.
- Spectral flux.
- Peak level.
- Crest factor.
- Attack and decay shape.
- Short-term RMS or envelope slope.
- Zero-crossing rate.
- Polarity or sign changes.
- Energy before and after the transient.

Advantages:

- Small and fast.
- Easy to inspect.
- Easy to export as weights and embed directly in Rust.
- No large inference runtime required.

### Proposal B — small decision tree

Try a shallow decision tree or small boosted tree if feature interactions are
important. Limit the depth and number of leaves so inference remains simple
and the decision can be inspected.

### Proposal C — tiny one-dimensional CNN

Use a small 1-D convolutional model only if the feature-based models cannot
separate clicks from speech. Train it on short raw-PCM or compact
feature-channel windows and export it to a format that can be evaluated in
the Rust pipeline without a large runtime.

The CNN is not the starting point. It adds deployment and validation
complexity and may overfit the available recordings.

## Runtime design

The model should not replace the entire squelch gate.

Recommended composition:

1. The existing squelch gate detects and mutes the noise floor.
2. A delayed PCM buffer supplies a short history around candidate events.
3. The classifier evaluates a candidate transient.
4. If the classifier identifies a click, apply a short, bounded gain envelope
   through the existing PCM filter seam.
5. If the event is classified as speech, pass it unchanged.

The correction should be a gain envelope or local replacement, not a rewrite
of the whole transmission. The model must be causal enough for live use,
with a documented look-ahead and total latency.

Embed small-model weights directly in Rust where practical. Avoid adding a
large ML dependency until a simpler model has failed.

## Candidate generation

The classifier should not necessarily run on every sample. Candidate
windows can be generated from a cheap transient detector using features such
as:

- Short-term level change.
- Peak or crest-factor threshold.
- Spectral flux.
- Difference between adjacent short-time envelopes.

Candidate generation must be permissive enough not to miss clicks. The
classifier, not an aggressive candidate gate, should provide the final
click-versus-speech decision.

## Evaluation

Overall classification accuracy is not sufficient. Measure:

- Click detection rate.
- False-negative rate for closing clicks.
- False-positive rate on speech.
- Detection latency.
- Added look-ahead latency.
- Residual click peak and perceived loudness.
- Speech attenuation caused by corrections.
- Behavior on clean transmission endings.
- Behavior after short pauses and short transmissions.
- Performance on recordings not used during development.

Listening tests remain required. Numerical peak reduction alone does not
show whether the result sounds natural.

The primary acceptance question is:

> Does the classifier reduce closing clicks on unseen recordings without
> producing audible damage to speech?

## Development sequence

1. Build and label a small multi-session corpus.
2. Define the candidate window and correction envelope without using the
   final evaluation recording.
3. Establish a non-ML feature baseline.
4. Train Proposal A, the linear classifier.
5. Evaluate it by recording/session, including hard speech negatives.
6. Try Proposal B only if the linear model is insufficient.
7. Try Proposal C only if the smaller models are insufficient.
8. Export the smallest model that meets the acceptance criteria.
9. Implement it behind a separate experimental flag.
10. Compare it with the interim `--declick` path using complete recordings.
11. Decide whether to retain the generic gate, use the classifier for closing
    clicks, or combine both.

## Open decisions

- Exact candidate window length.
- Whether the model should classify opening clicks, closing clicks, or both.
- Feature set and sample-rate preprocessing.
- Correction envelope duration and shape.
- Maximum acceptable inference latency.
- Maximum acceptable speech false-positive rate.
- Model export format or direct Rust implementation.
- Whether the classifier should be combined with or replace the generic
  closing fade.

No model architecture, threshold, feature set, or correction duration is
locked yet.
