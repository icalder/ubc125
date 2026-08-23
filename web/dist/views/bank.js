// Bank view (tabs 1-10): 50-row channel table with a cursor row,
// mirroring the console's bank screen.
import { el, box, setText, setClass, setDisabled } from "../components/box.js";
import { toDisplay } from "../lib/freq.js";

export const CHANNELS_PER_BANK = 50;

/** 1-based inclusive [start, end] channel index range for a 1-based bank. */
export function bankRange(bank) {
  return [(bank - 1) * CHANNELS_PER_BANK + 1, bank * CHANNELS_PER_BANK];
}

/**
 * Build the bank view once and return { update({ channels, cursor,
 * loading, rev }) }.
 *
 * `channels` is the full 500-slot array (stable reference, mutated in
 * place); `rev` increments whenever its contents change, so row text is
 * rewritten only on real data changes, not on every status tick. Rows and
 * buttons are stable nodes: the previous design re-created all 50 rows on
 * every tick, so a pointer press that straddled a tick landed its mouseup
 * on a replacement row and lost its click.
 */
export function createBank(container, bank, { onSelect, onEdit, onDelete }) {
  const [start] = bankRange(bank);

  const rows = [];
  const cells = [];
  for (let i = 0; i < CHANNELS_PER_BANK; i++) {
    const idx = el("span", { class: "col idx", text: String(start + i) });
    const name = el("span", { class: "col name" });
    const freq = el("span", { class: "col freq" });
    const mod = el("span", { class: "col mod" });
    const row = el("div", { class: "row", "data-index": String(i) }, idx, name, freq, mod);
    row.addEventListener("click", () => onSelect(i));
    rows.push(row);
    cells.push([idx, name, freq, mod]);
  }

  const table = el("div", { class: "table" },
    el("div", { class: "row header" },
      el("span", { class: "col idx", text: "Idx" }),
      el("span", { class: "col name", text: "Name" }),
      el("span", { class: "col freq", text: "Freq" }),
      el("span", { class: "col mod", text: "Mod" }),
    ),
    ...rows,
  );

  // Edit stays enabled on empty rows: that is the channel-creation path
  // (the console's `e` works on any row). Delete is disabled on empty rows
  // so we never issue DCH for an unprogrammed slot.
  const edit = el("button", { class: "btn", text: "e: Edit" });
  edit.addEventListener("click", onEdit);
  const del = el("button", { class: "btn danger", text: "d: Delete" });
  del.addEventListener("click", onDelete);

  container.replaceChildren(
    box(`Bank ${bank}`, {}, table),
    box("Actions", {}, el("div", { class: "actions" }, edit, del)),
  );

  let lastCursor = -1;
  let lastRev = -1;
  let chans = null;

  const writeIdx = (i, selected) => {
    const abs = start + i;
    setText(cells[i][0], selected ? `>> ${abs}` : String(abs));
  };
  const writeData = (i) => {
    const ch = chans[start + i];
    setText(cells[i][1], ch?.name || "");
    setText(cells[i][2], ch ? toDisplay(ch.frequency) : "");
    setText(cells[i][3], ch?.modulation || "");
  };

  return {
    update({ channels, cursor, loading, rev }) {
      if (channels !== chans) chans = channels;
      setClass(table, loading ? "table loading" : "table");
      if (rev !== lastRev) {
        lastRev = rev;
        for (let i = 0; i < CHANNELS_PER_BANK; i++) writeData(i);
      }
      if (cursor !== lastCursor) {
        if (lastCursor >= 0) {
          rows[lastCursor].classList.remove("selected");
          writeIdx(lastCursor, false);
        }
        rows[cursor].classList.add("selected");
        writeIdx(cursor, true);
        lastCursor = cursor;
      }
      setDisabled(del, !chans[start + cursor]);
    },
  };
}
