// Small DOM helpers shared by views. No state, no imports.

/** Create an element with optional class, text, and children. */
export function el(tag, opts = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(opts)) {
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = value;
    else if (key === "disabled" && value) node.disabled = true;
    else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2), value);
    }
  }
  for (const child of children) {
    if (child == null) continue;
    node.append(child);
  }
  return node;
}

/**
 * A titled box mimicking ratatui's Block: 1px border with the title
 * sitting on the top border line.
 */
export function box(title, opts = {}, ...children) {
  const inner = el("div", { class: `box-inner ${opts.class ?? ""}`.trim() }, ...children);
  return el("div", { class: `box ${opts.boxClass ?? ""}`.trim() },
    title ? el("span", { class: "box-title", text: title }) : null,
    inner,
  );
}

/** Replace a container's children with the given nodes. */
export function replace(container, ...children) {
  container.replaceChildren(...children);
}
