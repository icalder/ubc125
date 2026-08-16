// Pure frequency helpers mirroring src/types.rs (Frequency) and
// src/constants.rs. No DOM, no imports — testable with `node --test`.

const MHZ_DIGITS = 4;
const KHZ_DIGITS = 4;
const FREQ_DIGITS = 8;

/**
 * Convert user-entered frequency input to the scanner's 8-digit raw
 * string, or null if invalid. Mirrors `Frequency::from_user_input`.
 *
 * Accepts:
 * - "123.9750" — MHz.KHz (each part padded to 4 digits)
 * - "88.1"     — KHz right-padded to 4 digits
 * - ".1"       — MHz part omitted (treated as 0 MHz)
 * - "123."     — KHz part omitted (treated as 0 KHz)
 * - "01239750" — already 8 digits (raw form)
 * - "1239750"  — 7 digits (left-padded to 8)
 * - "123"      — short MHz (zero KHz)
 *
 * Returns e.g. "01239750", or null for non-numeric chars, multiple
 * dots, empty input, or oversized components.
 */
export function fromUserInput(input) {
  const s = String(input).trim();
  if (s.length === 0) return null;

  if ((s.match(/\./g) || []).length > 1) return null;
  if (!/^[0-9.]*$/.test(s)) return null;

  let raw;
  if (s.includes(".")) {
    const parts = s.split(".");
    let mhz = parts[0];
    let khz = parts[1] ?? "";

    if (mhz.length === 0 && khz.length === 0) return null;
    if (mhz.length > MHZ_DIGITS || khz.length > KHZ_DIGITS) return null;

    mhz = mhz.padStart(MHZ_DIGITS, "0");
    khz = khz.padEnd(KHZ_DIGITS, "0");
    raw = mhz + khz;
  } else if (s.length > FREQ_DIGITS) {
    return null;
  } else if (s.length >= 7) {
    raw = s.padStart(FREQ_DIGITS, "0");
  } else {
    raw = s.padStart(MHZ_DIGITS, "0") + "0000";
  }

  const n = Number(raw);
  if (!Number.isSafeInteger(n) || n >= 100_000_000) return null;
  return raw;
}

/**
 * Display an 8-digit raw string (or number) as `MHz.KHz`, with leading
 * zeros stripped from the MHz part. Mirrors `Display for Frequency`.
 * Empty/zero input displays as "".
 */
export function toDisplay(raw) {
  let n;
  if (typeof raw === "number") {
    n = raw;
  } else {
    const s = String(raw).trim();
    if (!/^[0-9]+$/.test(s) || s.length === 0) return "";
    n = Number(s);
  }
  if (n === 0) return "";
  const p = String(n).padStart(FREQ_DIGITS, "0");
  const mhz = p.slice(0, 4).replace(/^0+(?=\d)/, "") || "0";
  const khz = p.slice(4, 8);
  return `${mhz}.${khz}`;
}

/** True if the value is an empty (0) frequency. */
export function isEmpty(raw) {
  const n = typeof raw === "number" ? raw : Number(raw);
  return !Number.isFinite(n) || n === 0;
}
