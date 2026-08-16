// Monitor view (tab 0): Scanner Info, Live Scan, Actions, Active Banks.
// Mirrors render_monitor_view / render_bank_status in src/cmd/renderer.rs.
import { el, box, replace } from "../components/box.js";
import { toDisplay } from "../lib/freq.js";

function renderInfo(info) {
  const lines = [
    [`Model:   `, info.model],
    [`Version: `, info.version],
    [`Volume:  `, info.volume],
    [`Squelch: `, info.squelch],
  ];
  return box("Scanner Info", {},
    ...lines.map(([k, v]) =>
      el("div", {},
        el("span", { text: k }),
        el("span", { class: "value", text: v || "..." }),
      ),
    ),
  );
}

function renderLiveScan(status) {
  const signal = status?.signal_detected ?? false;
  const bank = status?.bank ? `Bank ${status.bank}` : "Bank -";
  const freq = toDisplay(status?.frequency ?? "") || "---";
  const name = status?.channel_name || "";

  return box("Live Scan", { boxClass: signal ? "signal" : "" },
    el("div", {},
      el("span", { class: "label", text: "Bank:" }),
      el("span", { class: "value", text: bank }),
    ),
    el("div", { class: "freq-line" },
      el("span", { class: "label", text: "Frequency:" }),
      el("span", { class: "value freq", text: `${freq} MHz` }),
    ),
    el("div", {},
      el("span", { class: "label", text: "Channel:" }),
      el("span", { class: "value name", text: name || "-" }),
    ),
  );
}

function renderActions(onScan, onHold) {
  const scan = el("button", { class: "btn", text: "s: Scan" });
  scan.addEventListener("click", onScan);
  const hold = el("button", { class: "btn", text: "h: Hold" });
  hold.addEventListener("click", onHold);
  return box("Actions", {}, el("div", { class: "actions" }, scan, hold));
}

function renderBanks(banks, onToggle) {
  const chips = banks.map((active, i) => {
    const chip = el("span", {
      class: `bank-chip ${active ? "on" : "off"}`,
      text: `[${(i + 1) % 10}]`,
    });
    chip.addEventListener("click", () => onToggle(i + 1));
    return chip;
  });
  return box("Active Banks (Press 1-0 to toggle)", {},
    el("div", { class: "banks" },
      el("span", { class: "label", text: "Banks: " }),
      ...chips,
    ),
  );
}

/** Render the monitor view into `container`. */
export function renderMonitor(container, { info, status, banks, onScan, onHold, onToggleBank }) {
  replace(container,
    renderInfo(info),
    renderLiveScan(status),
    renderActions(onScan, onHold),
    renderBanks(banks, onToggleBank),
  );
}
