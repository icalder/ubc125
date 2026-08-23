// Tab bar: "Monitor | Bank 1 | ... | Bank 10" (mirrors console tabs).
import { el, box, setClass } from "./box.js";

const TAB_LABELS = ["Monitor", ...Array.from({ length: 10 }, (_, i) => `Bank ${i + 1}`)];

/**
 * Build the tab bar once and return { update(selected) }.
 *
 * The tab nodes are stable across updates: the previous design re-created
 * every tab on each status tick, so a pointer press that straddled a tick
 * landed its mouseup on a replacement node and lost its click (Firefox
 * drops clicks whose mousedown node was removed).
 */
export function createTabs(container, onSelect) {
  const tabs = TAB_LABELS.map((label, i) => {
    const tab = el("span", { class: "tab", text: label });
    tab.addEventListener("click", () => onSelect(i));
    return tab;
  });
  container.replaceChildren(
    box("Tabs", { boxClass: "tabs-compact" },
      el("div", { class: "tabs" },
        ...tabs.flatMap((t, i) =>
          i === 0 ? [t] : [el("span", { class: "tab-sep", text: "|" }), t],
        ),
      ),
    ),
  );
  let current = -1;
  return {
    update(selected) {
      if (selected === current) return;
      current = selected;
      tabs.forEach((t, i) => setClass(t, i === selected ? "tab selected" : "tab"));
      // Keep the active tab visible without a scrollbar (phone-width tab
      // bar). Only on change: a scroll mid-press makes the browser cancel
      // the click.
      tabs[selected].scrollIntoView({ inline: "center", block: "nearest" });
    },
  };
}

export const NUM_TABS = TAB_LABELS.length;
