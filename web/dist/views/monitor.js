// Monitor view (tab 0): Scanner Info, Live Scan, Actions, Active Banks.
// Mirrors render_monitor_view / render_bank_status in src/cmd/renderer.rs.
import { el, box, setText, setClass, setDisabled } from "../components/box.js";
import { playIcon, stopIcon } from "../components/icons.js";
import { toDisplay } from "../lib/freq.js";

/** Chip label for a 1-based bank number; bank 10 shows `[0]` (console `bank_num % 10` quirk). */
export function bankLabel(bank) {
  return `[${bank % 10}]`;
}

/**
 * Build the monitor view once and return { update({ info, status, banks,
 * audio }) }.
 *
 * The structure (buttons, chips) is stable across updates: the previous
 * design re-created the whole view on every 250 ms status tick, so a
 * pointer press that straddled a tick landed its mouseup on a replacement
 * node and lost its click (Firefox drops such clicks outright). Dynamic
 * values update in place.
 */
export function createMonitor(container, { onScan, onHold, onToggleBank, onAudioPlay, onAudioStop }) {
  // Scanner Info: one value span per row, updated in place.
  const infoRows = [
    [`Model:   `, "model"],
    [`Version: `, "version"],
    [`Volume:  `, "volume"],
    [`Squelch: `, "squelch"],
  ];
  const infoSpans = infoRows.map(
    () => el("span", { class: "value", text: "..." }),
  );
  const infoBox = box("Scanner Info", {},
    ...infoRows.map(([label], i) =>
      el("div", {}, el("span", { text: label }), infoSpans[i]),
    ),
  );

  // Live Scan: the box border goes amber while a signal is detected.
  const liveBank = el("span", { class: "value" });
  const liveFreq = el("span", { class: "value freq" });
  const liveName = el("span", { class: "value name" });
  const liveBox = box("Live Scan", {},
    el("div", {},
      el("span", { class: "label", text: "Bank:" }),
      liveBank,
    ),
    el("div", { class: "freq-line" },
      el("span", { class: "label", text: "Frequency:" }),
      liveFreq,
    ),
    el("div", {},
      el("span", { class: "label", text: "Channel:" }),
      liveName,
    ),
  );

  // Actions: buttons are wired once and never replaced. data-key carries
  // the scanner key label and is the stable hook the E2E scripts select
  // on (play/stop render as icons, so their visible text is not stable).
  const scan = el("button", { class: "btn", "data-key": "s", text: "s: Scan" });
  scan.addEventListener("click", onScan);
  const hold = el("button", { class: "btn", "data-key": "h", text: "h: Hold" });
  hold.addEventListener("click", onHold);
  const play = el("button", { class: "btn", "data-key": "p", text: "p:" }, playIcon());
  play.addEventListener("click", onAudioPlay);
  const stop = el("button", { class: "btn", "data-key": "x", text: "x:" }, stopIcon());
  stop.addEventListener("click", onAudioStop);
  // The state span is what the E2E scripts read; its class carries the
  // state for colouring.
  const audioLabel = el("span", { class: "value" });
  const actionsBox = box("Actions", {},
    el("div", { class: "actions" }, scan, hold, play, stop),
    el("div", {},
      el("span", { class: "label", text: "Audio: " }),
      audioLabel,
    ),
  );

  // Active Banks: chips are wired once; only their on/off class moves.
  const chips = Array.from({ length: 10 }, (_, i) => {
    const chip = el("span", { class: "bank-chip off", text: bankLabel(i + 1) });
    chip.addEventListener("click", () => onToggleBank(i + 1));
    return chip;
  });
  const banksBox = box("Active Banks (Press 1-0 to toggle)", {},
    el("div", { class: "banks" },
      el("span", { class: "label", text: "Banks: " }),
      ...chips,
    ),
  );

  container.replaceChildren(infoBox, liveBox, actionsBox, banksBox);

  return {
    update({ info, status, banks, audio }) {
      infoRows.forEach(([, key], i) => setText(infoSpans[i], info[key] || "..."));
      const signal = status?.signal_detected ?? false;
      setClass(liveBox, signal ? "box signal" : "box");
      setText(liveBank, status?.bank ? `Bank ${status.bank}` : "Bank -");
      setText(liveFreq, `${toDisplay(status?.frequency ?? "") || "---"} MHz`);
      setText(liveName, status?.channel_name || "-");
      setDisabled(play, !audio.supported || audio.state !== "off");
      setDisabled(stop, audio.state === "off" || audio.state === "unavailable");
      const label = !audio.supported ? "not supported" : audio.state;
      setClass(audioLabel, `value audio-state audio-${label}`);
      setText(audioLabel, label);
      banks.forEach((active, i) => setClass(chips[i], active ? "bank-chip on" : "bank-chip off"));
    },
  };
}
