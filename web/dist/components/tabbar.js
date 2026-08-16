// Tab bar: "Monitor | Bank 1 | ... | Bank 10" (mirrors console tabs).
import { el, box, replace } from "./box.js";

const TAB_LABELS = ["Monitor", ...Array.from({ length: 10 }, (_, i) => `Bank ${i + 1}`)];

/** Render the tab bar into `container`. `selected` is 0-10. */
export function renderTabs(container, selected, onSelect) {
  const tabs = TAB_LABELS.map((label, i) => {
    const tab = el("span", {
      class: `tab ${i === selected ? "selected" : ""}`,
      text: label,
    });
    tab.addEventListener("click", () => onSelect(i));
    return tab;
  });
  replace(container,
    box("Tabs", {},
      el("div", { class: "tabs" },
        ...tabs.flatMap((t, i) =>
          i === 0 ? [t] : [el("span", { class: "tab-sep", text: "|" }), t],
        ),
      ),
    ),
  );
}

export const NUM_TABS = TAB_LABELS.length;
