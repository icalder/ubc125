// UBC125XLT web console — app state, keymap, gRPC-Web wiring.
// Mirrors the console TUI (src/cmd/console.rs + renderer.rs).
import { createClients, stripPrefix, errorMessage } from "./lib/client.js";
import { el } from "./components/box.js";
import { fromUserInput, toDisplay } from "./lib/freq.js";
import { renderTabs, NUM_TABS } from "./components/tabbar.js";
import { renderHelp } from "./components/helpbar.js";
import { renderMonitor } from "./views/monitor.js";
import { renderBank, CHANNELS_PER_BANK } from "./views/bank.js";
import { openEditModal, openConfirmDelete as showConfirmDelete } from "./components/modal.js";

const MAX_CHANNELS = 500;
const NUM_BANKS = 10;
const MAX_NAME_LEN = 16;

// ?server=http://host:port overrides the gRPC-Web endpoint (defaults to
// same origin, which works when served by `ubc125 serve`).
const serverParam = new URLSearchParams(location.search).get("server");
const { system, scanner } = createClients(serverParam ?? location.origin);

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
  connected: false,
  error: null, // persistent error (cleared by next action)
  flash: null, // transient info message
};

let modal = null; // { kind: "edit" | "delete", ... }
let flashTimer = 0;

// -- DOM -------------------------------------------------------------------

const appRoot = document.querySelector("#app");
const bannerRoot = document.createElement("div");
const tabRoot = document.createElement("section");
const viewRoot = document.createElement("section");
const helpRoot = document.createElement("section");
const modalRoot = document.createElement("div");
appRoot.append(bannerRoot, tabRoot, viewRoot, helpRoot, modalRoot);

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
  if (state.connected) {
    bannerRoot.replaceChildren();
  } else {
    bannerRoot.replaceChildren(
      el("div", { class: "offline-banner", text: "OFFLINE — waiting for scanner..." }),
    );
  }
  renderTabs(tabRoot, state.tab, setTab);
  if (state.tab === 0) {
    renderMonitor(viewRoot, {
      info: state.info,
      status: state.status,
      banks: state.banks,
      onScan: () => runScanAction(scanner.startScan({}), "Scan started"),
      onHold: () => runScanAction(scanner.holdScan({}), "Scan held"),
      onToggleBank: (bank) => toggleBank(bank),
    });
  } else {
    const [start] = bankRange(state.tab);
    renderBank(viewRoot, {
      bank: state.tab,
      channels: state.channels.slice(start, start + CHANNELS_PER_BANK),
      cursor: state.cursor - start, // 0-based within the bank
      loading: state.loadingBank === state.tab,
      onSelect: (i) => {
        state.cursor = (state.tab - 1) * CHANNELS_PER_BANK + 1 + i;
        render();
      },
      onEdit: openEdit,
      onDelete: openDeleteModal,
    });
  }
  renderHelp(helpRoot, state.tab, statusLine());
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
  let backoff = 1000;
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
        backoff = 1000;
        render();
      }
    } catch (err) {
      // Stream failed mid-iteration: keep the last status, retry below.
    }
    state.connected = false;
    render();
    await new Promise((r) => setTimeout(r, backoff));
    backoff = Math.min(backoff * 2, 15000);
  }
}

// -- banks / channels --------------------------------------------------------

function bankRange(bank) {
  return [(bank - 1) * CHANNELS_PER_BANK + 1, bank * CHANNELS_PER_BANK];
}

function clearBankRange(bank) {
  const [start, end] = bankRange(bank);
  for (let i = start; i <= end; i++) state.channels[i] = null;
  state.bankLoaded.delete(bank);
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
      flash("Quit is not available in the browser");
      render();
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
        openEdit();
        return;
      case "d":
        openDeleteModal();
    }
  }
}

window.addEventListener("keydown", onKeydown);

// -- init --------------------------------------------------------------------

render();
loadInfo();
streamStatus();
