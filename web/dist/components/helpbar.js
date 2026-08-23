// Bottom help/status bar (mirrors the console Help block).
import { el, box, setText, setClass } from "./box.js";

const HELP_MONITOR =
  "Use Left/Right to switch tabs. 's': Scan, 'h': Hold, '1-0': Toggle Banks, 'q': Monitor.";
const HELP_BANK =
  "Use Left/Right to switch tabs. Up/Down or j/k to navigate. 'e': Edit, 'd': Delete, 'q': Monitor.";

/**
 * Build the help bar once and return { update(tab, status) }.
 * `status` is { text, kind } where kind is one of
 * "normal" | "error" | "loading".
 */
export function createHelp(container) {
  const helpText = el("div", { class: "help-text" });
  const statusEl = el("div", { class: "status" });
  container.replaceChildren(box("Help", {}, helpText, statusEl));
  let currentTab = -1;
  return {
    update(tab, status) {
      if (tab !== currentTab) {
        currentTab = tab;
        setText(helpText, tab === 0 ? HELP_MONITOR : HELP_BANK);
      }
      const statusClass = { error: "status error", loading: "status loading" }[status.kind] ?? "status";
      setClass(statusEl, statusClass);
      setText(statusEl, `Status: ${status.text}`);
    },
  };
}
