// UBC125XLT web console — app state, keymap, gRPC-Web wiring.
// Mirrors the console TUI (src/cmd/console.rs + renderer.rs).
import { createClients, stripPrefix, errorMessage } from "./lib/client.js";
import { el } from "./components/box.js";
import { fromUserInput, toDisplay } from "./lib/freq.js";
import { INITIAL_BACKOFF, MAX_BACKOFF, nextBackoff } from "./lib/backoff.js";
import { createTabs, NUM_TABS } from "./components/tabbar.js";
import { createHelp } from "./components/helpbar.js";
import { createMonitor } from "./views/monitor.js";
import { AudioStream } from "./lib/audio.js";
import { createBank, CHANNELS_PER_BANK, bankRange } from "./views/bank.js";
import { openEditModal, openConfirmDelete as showConfirmDelete } from "./components/modal.js";

const MAX_CHANNELS = 500;
const NUM_BANKS = 10;
const MAX_NAME_LEN = 16;

// ?server=http://host:port overrides the gRPC-Web endpoint (defaults to
// same origin, which works when served by `ubc125 serve`).
const serverParam = new URLSearchParams(location.search).get("server");
const { system, audio: audioClient, scanner } = createClients(
  serverParam ?? location.origin,
);

// -- state -----------------------------------------------------------------

const state = {
  tab: 0, // 0 = Monitor, 1-10 = Bank N
  info: { model: "", version: "", volume: "", squelch: "" },
  status: null, // latest normalized GetStatusResponse
  banks: new Array(NUM_BANKS).fill(false),
  channels: new Array(MAX_CHANNELS + 1).fill(null),
  bankLoaded: new Set(), // banks whose channel list has been fetched
  cursor: 1, // selected channel index (1-500)
  loadingBank: 0, // bank currently being fetched (0 = none)
  channelRev: 0, // bumped whenever state.channels contents change
  connected: false,
  audio: "off", // AudioStream state (off|connecting|playing|reconnecting|unavailable)
  audioSupported: typeof MediaSource !== "undefined" &&
    MediaSource.isTypeSupported('audio/webm; codecs="opus"'),
  error: null, // persistent error (cleared by next action)
  flash: null, // transient info message
};

let modal = null; // { kind: "edit" | "delete", ... }
let flashTimer = 0;

// Audio keeps playing across tab switches; Stop is explicit.
const audioStream = new AudioStream(audioClient, {
  onState: (s) => {
    state.audio = s;
    render();
  },
});

// Test seam for the browser E2E (tests/web/web_audio_test.mjs): audibility
// is not observable from the DOM (the <audio> element is detached), so the
// scripts inspect its playhead through this handle.
window.__ubc125 = { audioStream };

// -- DOM -------------------------------------------------------------------

const appRoot = document.querySelector("#app");
const bannerRoot = document.createElement("div");
const tabRoot = document.createElement("section");
const viewRoot = document.createElement("section");
const helpRoot = document.createElement("section");
const modalRoot = document.createElement("div");
appRoot.append(bannerRoot, tabRoot, viewRoot, helpRoot, modalRoot);

// The tab bar and help bar are built once and updated in place; the active
// view is built on tab switches and updated in place on every status tick.
// The previous design re-created every button, chip, and table row on each
// 250 ms tick, so a pointer press that straddled a tick landed its mouseup
// on a replacement node and lost its click (Firefox drops such clicks
// outright — which is why keys, which have no mousedown/mouseup pair,
// always felt instant while the buttons felt slow).
const tabs = createTabs(tabRoot, setTab);
const help = createHelp(helpRoot);
let viewApi = null; // { tab, update } — the view currently mounted in viewRoot
let bannerConnected = null;

function flash(text) {
  state.flash = text;
  clearTimeout(flashTimer);
  flashTimer = setTimeout(() => {
    state.flash = null;
    render();
  }, 3000);
}

function statusLine() {
  if (state.error) return { text: state.error, kind: "error" };
  if (state.flash) return { text: state.flash, kind: "normal" };
  if (!state.connected) return { text: "Disconnected (retrying...)", kind: "loading" };
  if (state.loadingBank) return { text: `Loading... (Bank ${state.loadingBank})`, kind: "loading" };
  if (state.tab === 0) {
    return { text: state.status?.raw || "Waiting for status...", kind: "normal" };
  }
  return { text: "Ready", kind: "normal" };
}

function render() {
  if (state.connected !== bannerConnected) {
    bannerConnected = state.connected;
    bannerRoot.replaceChildren(
      state.connected ? [] : el("div", { class: "offline-banner", text: "OFFLINE — waiting for scanner..." }),
    );
  }
  tabs.update(state.tab);
  if (!viewApi || viewApi.tab !== state.tab) viewApi = mountView(state.tab);
  if (state.tab === 0) {
    viewApi.update({
      info: state.info,
      status: state.status,
      banks: state.banks,
      audio: { state: state.audio, supported: state.audioSupported },
    });
  } else {
    const [start] = bankRange(state.tab);
    viewApi.update({
      channels: state.channels,
      cursor: state.cursor - start, // 0-based within the bank
      loading: state.loadingBank === state.tab,
      rev: state.channelRev,
    });
  }
  help.update(state.tab, statusLine());
}

/** Build the view for `tab` into viewRoot; returns { tab, update }. */
function mountView(tab) {
  const api = tab === 0
    ? createMonitor(viewRoot, {
        onScan: () => runScanAction(scanner.startScan({}), "Scan started"),
        onHold: () => runScanAction(scanner.holdScan({}), "Scan held"),
        onToggleBank: (bank) => toggleBank(bank),
        onAudioPlay: () => audioStream.play(),
        onAudioStop: () => audioStream.stop(),
      })
    : createBank(viewRoot, tab, {
        onSelect: (i) => {
          state.cursor = (tab - 1) * CHANNELS_PER_BANK + 1 + i;
          render();
        },
        onEdit: openEdit,
        onDelete: openDeleteModal,
      });
  api.tab = tab;
  return api;
}

// -- gRPC ------------------------------------------------------------------

async function loadInfo() {
  state.error = null;
  try {
    const [model, version, audio, banks] = await Promise.all([
      system.getModelInfo({}),
      system.getFirmwareVersion({}),
      scanner.getAudioSettings({}),
      scanner.getEnabledBanks({}),
    ]);
    state.info.model = stripPrefix(model.result);
    state.info.version = stripPrefix(version.result);
    state.info.volume = stripPrefix(audio.volume);
    state.info.squelch = stripPrefix(audio.squelch);
    state.banks = banks.banks.slice(0, NUM_BANKS);
  } catch (err) {
    state.error = `Failed to load scanner info: ${errorMessage(err)}`;
  }
  render();
}

async function streamStatus() {
  let backoff = INITIAL_BACKOFF;
  for (;;) {
    try {
      for await (const s of scanner.getStatus({})) {
        state.connected = true;
        state.error = null;
        state.status = {
          frequency: fromUserInput(s.frequency) ?? "",
          bank: s.bank && s.bank !== "-" ? Number(s.bank) : null,
          channel_name: s.channelName,
          signal_detected: s.signalDetected,
          raw: s.rawResponse,
          modulation: s.modulation,
        };
        // The stream carries the server's current bank mask, so a
        // SetEnabledBanks from another tab (or a bank button pressed on
        // the unit) re-renders the Active Banks chips here.
        state.banks = s.banks.slice(0, NUM_BANKS);
        backoff = INITIAL_BACKOFF;
        render();
      }
    } catch (err) {
      // Stream failed mid-iteration: keep the last status, retry below.
    }
    state.connected = false;
    render();
    await new Promise((r) => setTimeout(r, backoff));
    backoff = nextBackoff(backoff, MAX_BACKOFF);
  }
}

/** True when the cursor is on an unprogrammed (empty) channel row. */
function selectedRowEmpty() {
  return !state.channels[state.cursor];
}

// -- banks / channels --------------------------------------------------------

function clearBankRange(bank) {
  const [start, end] = bankRange(bank);
  for (let i = start; i <= end; i++) state.channels[i] = null;
  state.bankLoaded.delete(bank);
  state.channelRev++;
}

async function loadBank(bank) {
  if (state.bankLoaded.has(bank)) return;
  state.loadingBank = bank;
  render();
  try {
    const resp = await scanner.listChannels({ bank });
    clearBankRange(bank);
    for (const c of resp.channels) {
      const frequency = fromUserInput(c.frequency) ?? "";
      state.channels[c.index] = {
        index: c.index,
        name: c.name,
        frequency,
        modulation: c.modulation,
      };
    }
    state.bankLoaded.add(bank);
    state.channelRev++;
  } catch (err) {
    state.error = `Failed to load bank ${bank}: ${errorMessage(err)}`;
  } finally {
    state.loadingBank = 0;
    render();
  }
}

function setTab(tab) {
  state.tab = tab;
  if (tab > 0) {
    const [start, end] = bankRange(tab);
    if (state.cursor < start || state.cursor > end) state.cursor = start;
    loadBank(tab);
  }
  render();
}

function moveCursor(dir) {
  const [start, end] = bankRange(state.tab);
  const next = state.cursor + dir;
  if (next >= start && next <= end) state.cursor = next;
  render();
}

async function toggleBank(digit) {
  const bank = digit === 0 ? NUM_BANKS : digit;
  state.error = null;
  const next = [...state.banks];
  next[bank - 1] = !next[bank - 1];
  try {
    await scanner.setEnabledBanks({ banks: next });
    state.banks = next;
    flash(`Bank ${bank} ${next[bank - 1] ? "enabled" : "disabled"}`);
  } catch (err) {
    state.error = `Failed to update bank mask: ${errorMessage(err)}`;
  }
  render();
}

async function runScanAction(rpc, okMsg) {
  state.error = null;
  try {
    await rpc;
    flash(okMsg);
  } catch (err) {
    state.error = `Failed: ${errorMessage(err)}`;
  }
  render();
}

// -- modals ------------------------------------------------------------------

function openEdit() {
  const index = state.cursor;
  const chan = state.channels[index];
  const handle = openEditModal(
    modalRoot,
    {
      index,
      frequency: chan ? toDisplay(chan.frequency) : "",
      name: chan?.name ?? "",
      field: "frequency",
    },
    {
      onSave: saveEdit,
      onCancel: () => {
        modal?.close();
        modal = null;
      },
    },
  );
  modal = { kind: "edit", index, ...handle };
}

async function saveEdit() {
  const { index, frequencyInput, nameInput, close } = modal;
  const frequency = fromUserInput(frequencyInput.value);
  if (!frequency) {
    state.error = "Invalid frequency";
    render();
    return;
  }
  const existing = state.channels[index];
  const bank = Math.ceil(index / CHANNELS_PER_BANK);
  try {
    await scanner.setChannel({
      channel: {
        index,
        name: nameInput.value.trim().slice(0, MAX_NAME_LEN),
        frequency,
        modulation: existing?.modulation || "AM",
      },
    });
    close();
    modal = null;
    clearBankRange(bank);
    loadBank(bank);
    flash(`Channel ${index} saved`);
    render();
  } catch (err) {
    close();
    modal = null;
    state.error = `Failed to save channel: ${errorMessage(err)}`;
    render();
  }
}

function openDeleteModal() {
  const index = state.cursor;
  const handle = showConfirmDelete(modalRoot, index, {
    onYes: confirmDelete,
    onNo: () => {
      modal?.close();
      modal = null;
    },
  });
  modal = { kind: "delete", index, ...handle };
}

async function confirmDelete() {
  const { index, close } = modal;
  const bank = Math.ceil(index / CHANNELS_PER_BANK);
  try {
    await scanner.deleteChannel({ index });
    close();
    modal = null;
    clearBankRange(bank);
    loadBank(bank);
    flash(`Channel ${index} deleted`);
    render();
  } catch (err) {
    close();
    modal = null;
    state.error = `Failed to delete channel: ${errorMessage(err)}`;
    render();
  }
}

// -- keymap ------------------------------------------------------------------

function onKeydown(e) {
  // Modal handling takes priority.
  if (modal) {
    if (modal.kind === "edit") {
      if (e.key === "Escape") {
        modal.close();
        modal = null;
      } else if (e.key === "Enter") {
        e.preventDefault();
        saveEdit();
      } else if (e.key === "Tab") {
        e.preventDefault();
        const inputs = [modal.frequencyInput, modal.nameInput];
        const i = inputs.indexOf(document.activeElement);
        modal.setActive(inputs[(i + 1) % inputs.length]);
      }
    } else if (modal.kind === "delete") {
      if (e.key === "y" || e.key === "Y") confirmDelete();
      else if (e.key === "n" || e.key === "N" || e.key === "Escape") {
        modal.close();
        modal = null;
      }
    }
    return;
  }

  switch (e.key) {
    case "q":
      setTab(0); // back to Monitor (no concept of quit in the browser)
      return;
    case "ArrowRight":
      setTab((state.tab + 1) % NUM_TABS);
      return;
    case "ArrowLeft":
      setTab((state.tab + NUM_TABS - 1) % NUM_TABS);
      return;
  }

  if (state.tab === 0) {
    switch (e.key) {
      case "s":
        runScanAction(scanner.startScan({}), "Scan started");
        return;
      case "h":
        runScanAction(scanner.holdScan({}), "Scan held");
        return;
      case "p":
        audioStream.play();
        return;
      case "x":
        audioStream.stop();
        return;
      default:
        if (/^[0-9]$/.test(e.key)) {
          e.preventDefault();
          toggleBank(Number(e.key));
        }
    }
  } else {
    switch (e.key) {
      case "ArrowDown":
      case "j":
        e.preventDefault();
        moveCursor(1);
        return;
      case "ArrowUp":
      case "k":
        e.preventDefault();
        moveCursor(-1);
        return;
      case "e":
      case "Enter":
        // Edit works on empty rows too — that is how new channels are programmed.
        openEdit();
        return;
      case "d":
        // Matches the disabled Delete button on empty rows.
        if (!selectedRowEmpty()) openDeleteModal();
    }
  }
}

window.addEventListener("keydown", onKeydown);

// -- init --------------------------------------------------------------------

render();
loadInfo();
streamStatus();
