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

- `web_hw_test.mjs` (W6, real scanner): W5 items 1–9 minus the offline
  part, plus the delete-restore round-trip on channel 63 (factory
  "BHX RADAR" 123.9750 AM): delete → row cleared → re-enter exact values
  → save → verified via `GetChannel`; bank-1 toggle off/on round-trip
  (state restored); all values left as found.

## Results

- 2026-08-16: W5 pointer path **23/23 pass** at 1280×720; offline banner
  + 390×844 checks **10/10 pass**.
- 2026-08-16 (UI review fixes): W5 pointer path **26/26 pass** (test made
  idempotent via the channel-53 restore round-trip; Delete-disabled-on-
  empty-row verified manually via CDP eval); offline banner + 390×844
  checks **10/10 pass** (stack script moved to `tests/ubc125_stack.sh`).
- 2026-08-16: W6 hardware pass **25/25 pass** (real scanner, round-trip
  writes only; channel 63 verified restored via grpcurl `GetChannel`).
