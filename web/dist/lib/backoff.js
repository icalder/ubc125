// Reconnect backoff schedule for the status stream (pure, unit-tested).
export const INITIAL_BACKOFF = 1000;
export const MAX_BACKOFF = 30000;

/** Next backoff delay: double the current one, capped at `cap` ms. */
export function nextBackoff(current, cap = MAX_BACKOFF) {
  return Math.min(current * 2, cap);
}

/** The backoff schedule from `start` until `count` delays are produced. */
export function backoffSequence(start = INITIAL_BACKOFF, count, cap = MAX_BACKOFF) {
  const delays = [];
  let current = start;
  for (let i = 0; i < count; i++) {
    delays.push(current);
    current = nextBackoff(current, cap);
  }
  return delays;
}
