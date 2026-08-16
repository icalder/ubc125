// Bank view (tabs 1-10): 50-row channel table with a cursor row,
// mirroring the console's bank screen.
import { el, box, replace } from "../components/box.js";
import { toDisplay } from "../lib/freq.js";

export const CHANNELS_PER_BANK = 50;

/** 1-based inclusive [start, end] channel index range for a 1-based bank. */
export function bankRange(bank) {
  return [(bank - 1) * CHANNELS_PER_BANK + 1, bank * CHANNELS_PER_BANK];
}

/**
 * Render the bank table into `container`.
 * `channels`: array of { name, frequency, modulation } or null, length 50.
 * `cursor`: 0-based index of the selected row.
 * `loading`: bank is still being fetched (table is dimmed).
 */
export function renderBank(container, { bank, channels, cursor, loading = false, onSelect, onEdit, onDelete }) {
  const selected = channels[cursor] ?? null;
  const rows = channels.map((ch, i) => {
    const selected = i === cursor;
    const abs = (bank - 1) * CHANNELS_PER_BANK + 1 + i;
    const idxText = selected ? `>> ${abs}` : `${abs}`;
    const row = el("div", {
      class: `row ${selected ? "selected" : ""}`,
      "data-index": String(i),
    },
      el("span", { class: "col idx", text: idxText }),
      el("span", { class: "col name", text: ch?.name || "" }),
      el("span", { class: "col freq", text: ch ? toDisplay(ch.frequency) : "" }),
      el("span", { class: "col mod", text: ch?.modulation || "" }),
    );
    row.addEventListener("click", () => onSelect(i));
    return row;
  });

  // Edit stays enabled on empty rows: that is the channel-creation path
  // (the console's `e` works on any row). Delete is disabled on empty rows
  // so we never issue DCH for an unprogrammed slot.
  const edit = el("button", { class: "btn", text: "e: Edit" });
  edit.addEventListener("click", onEdit);
  const del = el("button", { class: "btn danger", text: "d: Delete", disabled: !selected });
  del.addEventListener("click", onDelete);

  replace(container,
    box(`Bank ${bank}`, {},
      el("div", { class: `table${loading ? " loading" : ""}` },
        el("div", { class: "row header" },
          el("span", { class: "col idx", text: "Idx" }),
          el("span", { class: "col name", text: "Name" }),
          el("span", { class: "col freq", text: "Freq" }),
          el("span", { class: "col mod", text: "Mod" }),
        ),
        ...rows,
      ),
    ),
    box("Actions", {},
      el("div", { class: "actions" }, edit, del),
    ),
  );
}
