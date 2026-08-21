# Web UI browser E2E (W5 / W6)

Scripted Chrome/Edge session via CDP (browser-tools skill / puppeteer-core),
against the fake scanner — no hardware needed for W5. W6 is the same list
against the real scanner (`/dev/ttyACM0`), minus the offline simulation and
with round-trip-only writes.

Run with the browser tools skill (Edge with CDP on :9222):

```sh
# W5 stack (fake scanner) — idempotent, kills any old stack first:
bash tests/ubc125_stack.sh           # needs the debug binary built

# W6 stack (real scanner):
./target/debug/ubc125 serve --device /dev/ttyACM0 --server-addr 127.0.0.1:50051
```

`tests/ubc125_stack.sh` re-execs under `nix-shell -p socat` when `socat`
is not on PATH, and redirects all background stdio to logs — so it is safe
to run from a piped shell (the hiccup test does, via `execSync`).

Scripts (in `tests/web/`; need puppeteer-core — import it from the
browser-tools skill's node_modules, or `nix-shell -p nodejs` + local
install):

- `web_pointer_test.mjs` — 1280×720, **all interactions via CDP
  click/tap** (the pointer path):
  1. load `/` → model info shows UBC125XLT
  2. Live Scan box present with Frequency row (stream-driven)
  3. click Bank 2 tab → 50 rows, idx labels 51..100
  4. tap row 53 → selected (inverted `>> 53`)
  5. tap **Edit** → modal → set frequency + name → tap **Save** →
     "Channel 53 saved" → row shows new name (frequency is set explicitly
     so the step works when row 53 starts empty)
  6. tap row → **Delete** → **Yes** → "Channel 53 deleted" → row cleared
  7. re-save channel 53 (restore) — the fake keeps deletions across runs,
     so this keeps the test idempotent
  8. Monitor tab → tap bank chip [1] → toggle flash + class change
     (SCG write; tapped back to restore)
  9. tap **Scan** / **Hold** → flashes

- `web_hiccup_phone_test.mjs`:
  1. no banner while healthy; stop the server ~4s → **offline banner
     appears**; restart the stack → banner clears
     (note: the server keeps the GetStatus stream alive through transient
     poll errors by design, so the banner's trigger is the connection
     going down, not a GLG hiccup)
  2. 390×844 phone viewport: no horizontal overflow (monitor + bank
     views), action buttons ≥ 44 px, 50-row table renders, tap row →
     **Edit** → **Save** round-trip works by touch

- `web_two_tabs_test.mjs` (W5, **KI-2 regression**, ~25 s): two tabs
  against one serve; both must reach live GLG status, the OFFLINE banner
  must never appear on either during a 20 s observation, and both must
  still be live at the end. Pre-fix this failed with the banner flapping
  alternately on both tabs (49/80 and 24/80 samples) because each new
  `GetStatus` stream cancelled the previous singleton poller.

- `web_bank_sync_test.mjs` (W5, **bank-sync regression**, ~20 s): two
  tabs; a scan-bank chip toggled in tab 1 must appear in tab 2's
  "Active Banks" box within a 10 s window (and both stay live). The fake
  scanner persists the SCG bank mask across reads/writes so the change is
  what a re-read would see. Pre-fix the sync check failed: `state.banks`
  was loaded once on page load and the `GetStatus` stream carried no bank
  mask, so tab 2 kept the stale chips (9/11). The fix rides the shared
  status poller: `GetStatusResponse.banks` carries the server's current
  mask (fast-forwarded by `SetEnabledBanks`, slow-refreshed from the radio
  every 30 s), and the client updates `state.banks` per stream message —
  convergence is one poll (250 ms), far inside the 10 s window.

- `web_hw_test.mjs` (W6, real scanner): W5 items 1–9 minus the offline
  part, plus the delete-restore round-trip on channel 63 (factory
  "BHX RADAR" 123.9750 AM): delete → row cleared → re-enter exact values
  → save → verified via `GetChannel`; bank-1 toggle off/on round-trip
  (state restored); all values left as found.

- `web_audio_test.mjs` (W5, audio over gRPC — run under
  `node tests/web/web_audio_test.mjs` (needs a debug build of the binary;
  the stack script self-provisions socat);
  ~2 min). The script (re)starts the stack itself for each phase
  (`bash tests/ubc125_stack.sh` with `UBC125_AUDIO_CMD` set):
  1. Phase A — deterministic file: generates `/tmp/cap.webm` (60 s of tone
     from `ubc125 audio-tone` — the same muxer the Pi capture uses); stack
     runs `python3 tests/paced_file.py /tmp/cap.webm 4` (paced replay —
     raw `cat` outpaces the 48 KiB broadcast queue and the stream
     backpressure-errors before playback).
     audio defaults `off` (Play enabled, Stop disabled) → Play →
     `playing` → file ends → `reconnecting` → generation replays →
     `playing` again (late/reconnect clients get a fresh init) → Stop →
     `off`, capture process tree gone
  2. Phase B — continuous + throttled: stack runs `ubc125 audio-tone
     --loop --out -` (faster than real time); `emulateNetworkConditions`
     64 KiB/s download → broadcast lag → stream error → `reconnecting`
     observed (never `unavailable`) → unthrottle → `playing` → Stop, no
     leftover tone process
  3. Phase C — late joiner (second browser): with tab 1 playing, tab 2
     joins the running generation (trusted CDP click so autoplay really
     starts the element) → `playing`; the playhead is the ground truth
     (`window.__ubc125.audioStream._audio.currentTime` — the `<audio>`
     element is detached, audibility is not DOM-observable): it must be
     seeked into the stream (> 0 — pre-fix it stalls at 0 forever, see
     `lateJoinSeek`) and then advance; tab 1 stays `playing`
  4. Regression: `web_pointer_test.mjs` 26/26 and
     `web_hiccup_phone_test.mjs` 10/10 re-run green in the same session
     (both now open a fresh tab like the audio test — a stale-tab reuse
     intermittently dropped typed input).

## Results

- 2026-08-16: W5 pointer path **23/23 pass** at 1280×720; offline banner
  + 390×844 checks **10/10 pass**.
- 2026-08-16 (UI review fixes): W5 pointer path **26/26 pass** (test made
  idempotent via the channel-53 restore round-trip; Delete-disabled-on-
  empty-row verified manually via CDP eval); offline banner + 390×844
  checks **10/10 pass** (stack script moved to `tests/ubc125_stack.sh`).
- 2026-08-16: W6 hardware pass **25/25 pass** (real scanner, round-trip
  writes only; channel 63 verified restored via grpcurl `GetChannel`).
- 2026-08-18 (audio): W5 audio path **18/18 pass** (Phase A file + Phase B
  continuous/throttled; Edge 151, CDP :9222). Same session: W5 pointer path
  **26/26 pass**, offline banner + 390×844 checks **10/10 pass** (after
  switching all W5/W6 scripts to fresh tabs).
- 2026-08-19 (KI-2 fix, shared status poller): two-tab check **8/8 pass**
  (both tabs live, 0 OFFLINE-banner samples in 20 s). Negative control on
  the pre-fix build (`e1ace48`): banner flapping on both tabs — the check
  has teeth. Rust `cargo test`: 119 passed / 1 ignored.
- 2026-08-21 (KI-3 fix, late audio joiner seek): W5 audio path **22/22
  pass** — Phase C: the second browser joining a running generation is
  seeked to the earliest buffered data (playhead in-stream and advancing),
  first tab undisturbed. Pre-fix the late joiner showed "playing" with
  its playhead stalled at 0 — silent until the generation reset (the
  two-browser symptom reported on the Pi). Same session: W5 pointer path
  **26/26**, two-tab status **8/8**, web unit tests 33 pass.
- 2026-08-21 (bank-sync, pre-fix): `web_bank_sync_test.mjs` added —
  **9/11 pass** on the current build; the two tab-2 sync checks fail
  (tab 2's Active Banks chips never reflect tab 1's SCG write).
- 2026-08-21 (bank-sync fix, mask over the status stream): bank sync
  **11/11** (tab 2 converges within one 250 ms poll). Same session: W5
  pointer path **26/26**, two-tab status **8/8**, offline banner + 390×844
  checks **10/10**; Rust `cargo test`: 142 passed.
