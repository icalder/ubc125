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

function renderActions({ onScan, onHold, audio, onAudioPlay, onAudioStop }) {
  const scan = el("button", { class: "btn", text: "s: Scan" });
  scan.addEventListener("click", onScan);
  const hold = el("button", { class: "btn", text: "h: Hold" });
  hold.addEventListener("click", onHold);
  const play = el("button", { class: "btn", text: "p: Play" });
  play.disabled = !audio.supported || audio.state !== "off";
  play.addEventListener("click", onAudioPlay);
  const stop = el("button", { class: "btn", text: "x: Stop" });
  stop.disabled = audio.state === "off" || audio.state === "unavailable";
  stop.addEventListener("click", onAudioStop);
  // The state span is what the E2E scripts read; its class carries the
  // state for colouring.
  const label = !audio.supported ? "not supported" : audio.state;
  return box(
    "Actions",
    {},
    el("div", { class: "actions" }, scan, hold, play, stop),
    el("div", {},
      el("span", { class: "label", text: "Audio: " }),
      el("span", { class: `value audio-state audio-${label}`, text: label }),
    ),
  );
}

/** Chip label for a 1-based bank number; bank 10 shows `[0]` (console `bank_num % 10` quirk). */
export function bankLabel(bank) {
  return `[${bank % 10}]`;
}

function renderBanks(banks, onToggle) {
  const chips = banks.map((active, i) => {
    const chip = el("span", {
      class: `bank-chip ${active ? "on" : "off"}`,
      text: bankLabel(i + 1),
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
export function renderMonitor(container, { info, status, banks, audio, onScan, onHold, onToggleBank, onAudioPlay, onAudioStop }) {
  replace(container,
    renderInfo(info),
    renderLiveScan(status),
    renderActions({ onScan, onHold, audio, onAudioPlay, onAudioStop }),
    renderBanks(banks, onToggleBank),
  );
}
