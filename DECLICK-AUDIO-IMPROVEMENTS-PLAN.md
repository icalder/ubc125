# De-Clicker Audio-Quality Improvements — Plan

**Status:** proposal — implement, A/B, and (only after user approval) lock.

This plan improves the *voice quality* of the locked wavelet de-clicker
(`WAVELET-DECLICK-PLAN.md`, v5c / 171 ms; core in `src/audio/declick.rs`)
without weakening click suppression. Three independent, composable
proposals (A: soft-knee replacement, B: post-peak tail confirmation,
C: per-level K), plus the exact harness/test procedure.

**Read first (in this order):**

1. `WAVELET-DECLICK-PLAN.md` §2 (click catalogue + voice regions —
   ground truth), §3–4 (why wavelets work; locked config), §8 (pitfalls).
2. This document.
3. `src/audio/declick.rs` (module docs = the locked semantics) and
   `examples/wavelet_declick.rs` (the A/B harness).

**Rules of the engagement** (from the lock process):

- The locked path stays **bit-exact** while all new features are off.
  New behavior is gated behind new configuration; the default
  configuration *is* the locked configuration.
- Every change is validated by a fresh A/B on
  `test-audio/unfiltered.wav` with `examples/wavelet_declick.rs`, and the
  **user decides by ear** (the lock was ear-selected). Numbers are
  supporting evidence, not the arbiter.
- Do not touch the locked constants (`LVL`, `LVLS`, `WIN_MS`, the
  std@6/MAD@7–10 hybrid, `BLOCK`/`HOP`, f64, the trailing-window
  semantics in `LevelState::process`). New parameters are new constants.
- `test-audio/*.wav` are gitignored — regenerate references before
  re-validating (see `test-audio/README.md`).

---

## 1. Context

### 1.1 The problem

With the locked v5c/171 ms config, clicks are well suppressed
(measured −2.4…−6.7 dB per click), but **voice quality is affected**:
speech — especially plosives/fricatives and the trill cluster — sounds
attenuated/muffled. The mechanism: the replacement rule is a *hard*
swap of any coefficient exceeding `K·σ` (K=3) with the local baseline
(mean at d6, median at d7–d10). Every flag is a local notch in the
reconstructed audio. On the reference capture the locked offline pass
flags 1137 coefficients; **297 (26 %) land inside voice regions**
(measured; script below). d6 (375–750 Hz) carries ~80 % of voice
energy, so marginal flags there are audible.

### 1.2 Why not the Lipschitz plan (`LIPSCHITZ-DECLICK-PLAN.md`) — evidence

The Lipschitz proposal (a second-stage "α < 0 ⇒ click" gate on the
scale-slope of the wavelet modulus) was evaluated against the repo's own
reference capture before implementation. `tools/lipschitz_check.py`
reproduces the locked offline amplitude test, computes the plan's α at
every flagged coefficient, and labels each by ground truth. Results:

```
class       n    alpha(point) min/med/max    #alpha<0
click-A    149    -3.04 / +0.37 / +3.48        62  (42%)
click-B    212    -2.35 / +0.16 / +3.63       103  (49%)
click-C     54    -2.72 / -0.58 / +2.68        34  (63%)
voice      297    -3.73 / -1.92 / +3.99       285  (96%)
other      425    -3.59 / -1.34 / +3.95       337  (79%)
```

- The plan's premise ("click = singularity, α ≈ −1; plosive = smooth,
  α > 0") is **false for this scanner**: the measured click is a
  1 ms step / 3–5 ms ramp + 50–140 ms smooth biphasic tail — a
  *step-like* transient, and step-like transients measure **negative**
  α in the plan's own DWT-level fit (impulses, by contrast, measure
  positive — the CWT convention was transplanted without flipping the
  axis).
- **Voice flags are *more* negative than click flags** (median −1.92
  vs +0.16/+0.37): a plosive attack is also step-like (synthetic
  plosive: α ≈ −5/−3.5). The two populations overlap; the gate has no
  separating power.
- Consequences if shipped as written: the plosives that are being
  damaged stay suppressed (no quality gain), while the click's smooth
  50–140 ms tail — 95–98 % of family-B energy — has α > 0 and would be
  **kept** (click suppression regresses).

Run it yourself (needs the reference WAVs present):

```sh
nix-shell -p python3Packages.pywavelets python3Packages.numpy \
  --command 'python3 tools/lipschitz_check.py'
```

**Conclusion:** the independent axis for *this* click is temporal, not
scale-structural. The template-era work already found it: "Syllable
onsets score up to 0.98 against a click attack kernel — only the
post-peak tail separates clicks from speech" (`test-audio/README.md`).
Proposal B below builds the second stage on that measured
discriminator instead.

### 1.3 The asymmetry everything exploits

Click excursions in coefficient space are **huge** relative to the
local σ (click peak 0…−0.5 dBFS against a −41 dBFS floor ⇒ tens to
hundreds of σ at d9/d10). Voice transients that get flagged sit just
above the knee, typically **3–5 σ**. Any rule that treats the two
populations differently by excursion size is working with a ~20 dB
margin.

---

## 2. Proposals

All three modify the *replacement decision* in `src/audio/declick.rs`
(today: `apply_threshold` — `if |v − m| > K·σ { out = m } else { out = v }`).
They are independent knobs; A/B them separately, then in combination
(§5 step 5).

### 2.1 Proposal A — soft-knee replacement with a hard upper knee

**Motivation.** The hard swap removes the *entire* deviation of a
3.2σ voice flag, while a click is 30–100σ. Retain most of a marginal
excursion; only fully replace excursions that are unambiguously click.

**Rule** (per coefficient, per level k). Let `dev = v − m`,
`x = |dev|`, `T = K_k·σ` (K_k = 3.0 initially), `T_hi = K_HI·σ`
(default `K_HI = 8.0`), `ρ` ∈ [0, 1) (default `0.25`). Retained
deviation `r(x)`:

```
r(x) = x,                                  x ≤ T            (pass — locked)
       T + ρ·(x − T),                      T < x ≤ T_hi     (soft knee)
       (T + ρ·(T_hi − T)) · (2·T_hi − x)/T_hi,   T_hi < x < 2·T_hi  (ramp to full removal)
       0,                                  x ≥ 2·T_hi       (locked hard replacement)

out = m + sign(dev) · r(x)
```

- Continuous at `T` and `T_hi`, monotone; `ρ = 1` ⇒ `r(x) = x` for
  `x ≤ T_hi` (implement `ρ ≥ 1.0` as *no replacement at all* — it is
  the transparency sanity check, and avoids the f64 round-trip
  `m + (v−m) ≠ v` issue).
- Behavior: a 3.2σ voice flag keeps 3.05σ (dent 0.15σ, vs the locked
  full 3.2σ removal); an 8σ flag keeps 4.25σ; anything ≥ 16σ is fully
  replaced exactly as locked — **click removal is unchanged**
  (clicks sit far above 2·T_hi).
- Special cases: `ρ = 0` = hard clip to `m ± T` (an A/B alternative);
  `K_HI → ∞` = pure soft knee.

**Parameters:** `SOFT_RHO: f64 = 0.25`, `K_HI: f64 = 8.0` (new
constants; feature off by default).

**A/B grid:** ρ ∈ {0.0, 0.25, 0.5} × K_HI ∈ {6, 8, 12} (9 outputs;
ρ=0 is the clip variant). If the user finds the soft knee too gentle,
lower ρ; if clicks survive, lower K_HI — never raise the flagging K.

### 2.2 Proposal B — post-peak tail confirmation (second-stage gate)

**Motivation.** The measured discriminator: a click's signature is the
**biphasic tail** — a sign flip in the same band 50–140 ms after the
peak. Speech transients (plosive bursts, fricatives, trill closures)
have sharp onsets but *no* biphasic tail. Gate the replacement on the
tail. This is the structural second stage the Lipschitz plan wanted to
be, built on the feature with actual measured power.

**Rule** (per flagged coefficient at level k, index i, deviation dev).
Search the *same level's* coefficients for a sign flip:

```
window:  j ∈ (i, i + w_tail]   where w_tail = round(TAIL_WIN_MS · FS/1000 / 2^k)
         (j must be < len(d))
floor:   a flip is a coefficient d[j] with sign(d[j]) == −sign(dev)
         and |d[j]| ≥ max(TAIL_FLOOR_SIGMA · σ_j, 0.1 · |dev|)
         (σ_j = that coefficient's own local window σ, same stats fn)
decision: flip found          ⇒ confirm  (replace, per Proposal A/locked rule)
         no flip / window does not fit the block ⇒ confirm (locked behavior)
         window fits, no flip  ⇒ veto     (keep v)
```

**Parameters:** `TAIL_WIN_MS: f64 = 80.0` (measured tail 50–140 ms;
80 ms is chosen because it fits inside the 171 ms block — see
causality), `TAIL_FLOOR_SIGMA: f64 = 1.5`.

**Causality / latency — no change.** The gate runs *inside*
`process_block`, which already sees the full 171 ms block's
coefficient arrays. A flag at block-relative time `p` can confirm only
if `p + TAIL_WIN_MS ≤ 171 ms`; otherwise it confirms by default
(locked behavior). With an 80 ms window, a flag's *later* covering
block (the one where its relative position < 85.5 ms) always has room,
so every flag is checkable in at least one of its two OLA blocks; the
effect is position-weighted by the OLA crossfade (strongest near each
block's end). No extra buffering, no added latency, the 214 ms
end-to-end delay is unchanged. In `declick_offline` (whole-signal, no
blocks) the window always fits except at the signal end.

**Expected effect (set expectations):** because the OLA averages two
blocks and the gate only vetoes where the window fits, a vetoed flag
is attenuated by ~50–100 % position-dependently, not fully removed.
The goal is *voice* protection (trill cluster 11.675–11.718 s,
plosives in 6–7 s / 12–12.5 s), not extra click removal. Watch the
per-click lines in the report: family-C (5.433 s, oddball) is the one
to watch for a missed tail (its shape is single-member; if the gate
lets it through, that is a known limitation, not a bug).

**A/B grid:** TAIL_WIN_MS ∈ {60, 80, 100} × TAIL_FLOOR_SIGMA ∈
{1.0, 1.5, 2.0} (9 outputs).

### 2.3 Proposal C — per-level K

**Motivation.** The click's energy is low: family B carries 95–98 %
below 94 Hz (i.e. d9–d10 + a10; `tools/wavelet_probe.py`), and the
1–5 ms edge sits mainly higher (d1–d5, not thresholded; some spill
into d6–d7 — see the risk note below). So d6 (375–750 Hz) and
d7 (187–375 Hz) carry only ~2–5 % of click energy — yet d6 carries
~80 % of *voice* energy and its 21 ms window makes it the most
flag-happy level. Raise K where the voice is, keep K=3 where the click
is.

**Rule:** per-level threshold `K_k` replaces the uniform `K = 3.0` in
the flagging decision (and in Proposal A's `T`). A/B candidate values
for (K6, K7) with (K8, K9, K10) = (3, 3, 3):

```
(3.5, 3.5)   (3.5, 4.0)   (4.0, 4.0)   (4.0, 4.5)
```

**Risk to watch:** a higher K6/K7 lets the 375–1500 Hz part of the
click *onset/edge* through — the per-click dB lines in the report will
show it if it matters. If click reduction drops by more than ~1 dB on
any click, back off that level.

### 2.4 Expected end state

Likely combination: **A (ρ ≈ 0.25, K_HI ≈ 8) + B (80 ms, 1.5σ)**,
optionally + C (K6/K7 ≈ 3.5). A fixes the per-flag artifact, B fixes
the flag *decision* for the tailless speech transients, C reduces flag
*count* in the voice band. A alone is the quick win; B alone is the
most principled.

---

## 3. Core changes — `src/audio/declick.rs`

### 3.1 Configuration type

```rust
/// All-new knobs. `default()` == the locked v5c behavior exactly.
#[derive(Clone, Copy, Debug)]
pub struct DeclickConfig {
    /// Per-level flagging K (index i ↔ LVLS[i]); locked = 3.0 all.
    pub k: [f64; 5],
    /// Proposal A: None = locked hard replacement. Some((rho, khi)).
    pub soft: Option<(f64, f64)>,
    /// Proposal B: None = no tail gate. Some((win_ms, floor_sigma)).
    pub tail: Option<(f64, f64)>,
}
```

`Default` impl: `k: [3.0; 5]`, `soft: None`, `tail: None`.

**Bit-exact rule:** when `soft` and `tail` are both `None`, the code
must execute the *existing* replacement path unchanged (a branch, not
a degenerate new formula — f64 rounding of `m + sign·r(x)` is not the
same as `m`). Keep `apply_threshold` as the locked path; add
`apply_threshold_cfg(c, stat, k, cfg)` used only when the feature is on.

### 3.2 API surface

- `DeClicker::with_config(cfg)`; `DeClicker::new()` delegates with
  `DeclickConfig::default()` (existing callers — `DeClickFilter`,
  tests — are untouched).
- `DeClickFilter::with_config(cfg)` likewise; `for_capture` carries the
  config.
- `declick_offline(x)` keeps its signature; add
  `declick_offline_cfg(x, cfg)`.
- **Expose flags** (needed by the harness's new report lines): change
  `process_block` internals to collect per-level flagged indices
  (`Vec<(usize, Vec<usize>)>`, level + positions) and add
  `process_block_with_flags(&mut self, sig) -> Result<(Vec<f64>, Vec<(usize, Vec<usize>)>), String>`;
  `process_block` delegates and discards. Extend `declick_offline_cfg`
  to return positions as well as counts.

### 3.3 Implementation notes

- The tail gate is a **second pass** over the flagged indices (flag
  collection is already there — today it only counts). For each flag,
  the floor needs `σ_j` at future indices `j > i`: reuse the same
  `stat(j)` closure (streaming: the trailing-window stats — the
  prefix up to `j` is available inside `process_block`, so this is
  block-causal and correct).
- `w_tail = (TAIL_WIN_MS · FS/1000 / 2^k).round() as usize` (≥ 1);
  window fits iff `i + w_tail <= d.len()` **and** the window's end
  time ≤ block end (in streaming: `(i + w_tail) · 2^k ≤ BLOCK`
  samples; in offline: only the signal end matters).
- No new allocations in the hot path beyond what flag collection
  already does; the tail scan is O(w_tail) per flag. Budget check:
  ~1137 flags over the 20 s signal × ≤ 60 coeffs (80 ms @ d6 =
  3840 samples / 2⁶) ≈ 7·10⁴ ops total — negligible against the
  ~0.6 ms p99 per block (verify in the report's p99 line).
- **Unit tests to add** (in `declick.rs` `mod tests`, synthetic
  coefficient arrays — no WAVs needed):
  1. `soft_knee_retains_fraction_of_excess` — one 4σ flag,
     ρ=0.5, K_HI=8 ⇒ out == m + 0.5·(4σ−3σ)·sign (exact).
  2. `soft_knee_hard_above_2khi` — one 20σ flag ⇒ out == m (locked).
  3. `soft_knee_continuous_at_knees` — coefficients at 2.999σ /
     3.001σ / 7.999σ / 8.001σ give a monotone, bounded-jump-free
     `r(x)` (assert |out(7.999σ) − out(8.001σ)| < 0.01σ).
  4. `tail_gate_vetoes_flag_without_flip` — flag at i, all
     `d[i+1..i+w_tail]` same-sign or below floor ⇒ out == v.
  5. `tail_gate_confirms_flag_with_flip` — flag at i, one opposite-sign
     coefficient ≥ floor inside the window ⇒ replaced per the
     active rule.
  6. `tail_window_clipped_confirms` — flag near block end where the
     window does not fit ⇒ locked behavior (replaced).
  7. `rho_ge_1_is_transparent` — ρ = 1.0 ⇒ output == input, bit-exact.
  8. Existing tests must stay green unmodified (they exercise the
     locked `new()`/`default()` path).

---

## 4. Harness changes — `examples/wavelet_declick.rs`

### 4.1 CLI

Keep `offline | rt` modes and their positional args; append optional
`--key value…` options (parse with a plain loop over `args`; no
clap — the example has no flag parsing today):

```
cargo run --release --example wavelet_declick rt 171 [out.wav]
    [--soft RHO KHI] [--k K6 K7 K8 K9 K10] [--tail WIN_MS FLOOR_SIGMA] [--ref FILE]
cargo run --release --example wavelet_declick offline [out.wav] [same options]
```

- No options ⇒ locked config ⇒ output must be bit-identical to the
  committed reference (regression, §5 step 0).
- Options combine (A + B + C). The first line of output must print the
  active config (e.g. `config: k=[4,3.5,3,3,3] soft=(0.25, 8) tail=(80, 1.5)`)
  so every report is self-documenting.
- `--ref FILE` adds a `max|d| / rms|d|` comparison line against an
  arbitrary WAV (use it to compare against the locked
  `test-audio/wavelet_rs_rt171ms.wav`). The built-in comparisons
  (`compare_refs`) stay.

### 4.2 Output naming

Explicit `out.wav` positional (already supported). Conventional names
for this round (all in `test-audio/`, gitignored):

```
wavelet_rs_rt171ms_soft_rho{RHO}_khi{KHI}.wav     # Proposal A grid
wavelet_rs_rt171ms_k{K6}_{K7}.wav                 # Proposal C grid
wavelet_rs_rt171ms_tail{WIN}ms_f{FLOOR}.wav       # Proposal B grid
wavelet_rs_rt171ms_combo.wav                      # final combination
```

### 4.3 Report additions (in `report()`)

Existing lines (per-click peak ±10 ms, per-voice-region RMS, max
level) stay. Add:

1. **Per-voice-region `max|Δ|`** (dBFS of max |y−x| in the region) —
   the worst single dent; the trill cluster (11.675–11.718 s) is the
   most sensitive and already in the VOICE list.
2. **Flag census** (from the new flag-exposing API): total flags,
   flags per level, and flags per voice region / per click window /
   other — the "297 voice flags" number must be reproducible from the
   harness, not just from `tools/lipschitz_check.py`. Coefficient
   center time ≈ `i · 2^k / FS` (a few-sample phase offset is fine for
   region counting).
3. Keep the rt mode's p99/RTF lines (performance regression guard).

### 4.4 Baseline numbers to beat (locked, from the validated state)

```
rt 171: clicks −2.4 … −6.7 dB (per-click lines), voice loss ≤ −1.8 dB,
        RTF ≈ 0.006, p99 block ≈ 0.6 ms (vs 85.5 ms hop)
offline: matches wavelet_v5c_v2awin.wav to −20.9 dBFS max
```

---

## 5. Test procedure (step-by-step)

**Environment.** `test-audio/unfiltered.wav` + reference WAVs must be
present (gitignored — regenerate per `test-audio/README.md` if
missing; they are needed by the unit tests too, which skip gracefully
when absent). Python for cross-checks:
`nix-shell -p python3Packages.pywavelets python3Packages.numpy --command …`.

**0. Baseline (do this before any code change).**

```sh
cargo test                                   # all green, incl. the bit-exact offline-simulation test
cargo run --release --example wavelet_declick rt 171 /tmp/locked_baseline.wav
cmp /tmp/locked_baseline.wav test-audio/wavelet_rs_rt171ms.wav   # must be identical
```

Save the printed click/voice table — it is the reference for every
A/B table in this round. (If `cmp` fails because the committed
reference is stale, regenerate it with the current locked code,
record that in the progress log, and use the fresh file as baseline.)

**1. Proposal A.** Implement §3 (config type + soft knee only) + §4
(harness `--soft`, report additions). Then:

```sh
for rho in 0.0 0.25 0.5; do
  for khi in 6 8 12; do
    cargo run --release --example wavelet_declick rt 171 \
      test-audio/wavelet_rs_rt171ms_soft_rho${rho}_khi${khi}.wav \
      --soft $rho $khi --ref test-audio/wavelet_rs_rt171ms.wav
  done
done
```

Screen the printed tables: (a) every per-click line must stay within
~1 dB of the locked baseline's per-click line; (b) voice-region
`max|Δ|` and the voice-region flag dents should improve, worst at the
trill cluster. Then listen:

```sh
paplay test-audio/wavelet_rs_rt171ms.wav                 # locked
paplay test-audio/wavelet_rs_rt171ms_soft_rho0.25_khi8.wav
```

Focus: plosives in 6–7 s and 12–12.5 s; the trill at 11.68 s; the
loudest clicks at 2.027/2.297 s (they must still be gone). Ask the
user. Pick the best ρ/K_HI (or reject A).

**2. Proposal C.** Add `--k` (per-level K in the flagging decision).
Run the §2.3 grid with the locked replacement, then re-run the best
A+C pair. Same screen as step 1; the specific risk is click-onset
residual in d6/d7 — check the per-click lines first.

**3. Proposal B.** Add `--tail` (gate per §2.2). Run the §2.2 grid.
Screen: voice-region flag census must drop (the tailless trill/plosive
flags are vetoed); per-click lines must not degrade by more than
~1 dB; family C (5.433 s) is the canary. Listen as in step 1.

**4. Performance + regression after every implementation step.**

```sh
cargo test
cargo run --release --example wavelet_declick rt 171 /tmp/locked_check.wav
cmp /tmp/locked_check.wav test-audio/wavelet_rs_rt171ms.wav       # features off ⇒ bit-exact
```

The report's p99 line must stay ≪ 85.5 ms (headroom was ~140×).

**5. Combination.** Best A + B (+ C) in one run:

```sh
cargo run --release --example wavelet_declick rt 171 \
  test-audio/wavelet_rs_rt171ms_combo.wav \
  --soft <rho> <khi> --tail <win> <floor> [--k …] \
  --ref test-audio/wavelet_rs_rt171ms.wav
```

Also run `offline` mode with the same config (offline = whole-signal
windows; the tail gate is fully active there — the numbers should be
at least as good as rt). Full listen A/B: unfiltered → locked → combo.
**User decides.**

**6. Success criteria (for the combination, before it becomes a lock
candidate):**

- Voice: trill-cluster and plosive-region `max|Δ|` measurably reduced
  vs locked (or the user hears a clear improvement — the ear wins).
- Clicks: no click worse than locked by more than ~1 dB; ideally
  unchanged.
- Locked path (all features off): bit-exact (unit test + `cmp`).
- RTF/p99: unchanged within noise (≪ 1 ms p99).
- `streaming_filter_is_transparent_to_a_tone` and the offline-
  simulation bit-exact test still green.

**7. If approved (out of scope until then):** lock the new
configuration in `WAVELET-DECLICK-PLAN.md` §3/§4 (superseding the
replacement rule and, if C won, the uniform K), append the progress
log (§10), wire `serve`/`UBC125_DECLICK*`/Nix options mirroring the
existing `--declick` plumbing (the `DeClickFilter::with_config` seam
is already in place), and update `AGENTS.md`'s audio paragraph. Delete
or mark `LIPSCHITZ-DECLICK-PLAN.md` as rejected-with-evidence (pointer
to §1.2 and `tools/lipschitz_check.py`).

---

## 6. Pitfalls (this round)

- **Do not "fix" the trailing-window semantics** in
  `LevelState::process` (whole-prefix window is the validated
  behavior) or the std@6/MAD@7–10 hybrid — both are locked for
  measured reasons.
- **f64, same operation order** where the locked path runs — the
  bit-exact regression is the contract.
- The tail gate's default-on-miss is **confirm** (locked behavior),
  never "veto by default" — a missed flip must not *add* suppression.
- `w_tail = round(80 ms · FS/1000 / 2^k)`: 60 coefficients at d6,
  30 at d7, 15 at d8, 8 at d9, 4 at d10 — size the scan loop
  accordingly (no `Vec` per flag; index arithmetic only).
- The OLA means a veto in one covering block only partially removes a
  flag (§2.2) — don't read single-block unit-test expectations as
  end-to-end attenuation.
- `test-audio/*.wav` are gitignored: every run's numbers are only
  reproducible with the same `unfiltered.wav`. If it is regenerated,
  re-run step 0 and re-baseline *all* tables.

## 7. Reference

| what | where |
|---|---|
| Locked config + ground truth | `WAVELET-DECLICK-PLAN.md` §2–4, §8 |
| Core (locked pipeline) | `src/audio/declick.rs` |
| A/B harness | `examples/wavelet_declick.rs` |
| Reference capture + regeneration | `test-audio/` + `test-audio/README.md` |
| Lipschitz evidence (this plan's §1.2) | `tools/lipschitz_check.py` |
| Scale-energy evidence (clicks < 94 Hz) | `tools/wavelet_probe.py` |
| Rejected structural alternative | `LIPSCHITZ-DECLICK-PLAN.md` |
