// Bottom help/status bar (mirrors the console Help block).
import { el, box, replace } from "./box.js";

const HELP_MONITOR =
  "Use Left/Right to switch tabs. 's': Scan, 'h': Hold, '1-0': Toggle Banks, 'q': Monitor.";
const HELP_BANK =
  "Use Left/Right to switch tabs. Up/Down or j/k to navigate. 'e': Edit, 'd': Delete, 'q': Monitor.";

/**
 * Render the help bar. `status` is { text, kind } where kind is one of
 * "normal" | "error" | "loading".
 */
export function renderHelp(container, tab, status) {
  const help = tab === 0 ? HELP_MONITOR : HELP_BANK;
  const statusClass = { error: "status error", loading: "status loading" }[status.kind] ?? "status";
  replace(container,
    box("Help", {},
      el("div", { class: "help-text", text: help }),
      el("div", { class: statusClass, text: `Status: ${status.text}` }),
    ),
  );
}
