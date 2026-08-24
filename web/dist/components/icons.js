// Inline SVG icons for the action buttons.
//
// Built with createElementNS: box.js's `el` uses document.createElement,
// which creates elements in the HTML namespace that browsers refuse to
// render as SVG. The <title> is the icon's accessible name (and the
// browser tooltip); the shape's fill comes from the icon-<name> class in
// theme.css.

const SVG_NS = "http://www.w3.org/2000/svg";

/** A 24x24 icon: a <title> plus a single shape element. */
function icon(name, className, shape) {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("class", `icon ${className}`);
  svg.setAttribute("viewBox", "0 0 24 24");
  const title = document.createElementNS(SVG_NS, "title");
  title.textContent = name;
  svg.append(title, shape);
  return svg;
}

/** Green play triangle. */
export function playIcon() {
  const poly = document.createElementNS(SVG_NS, "polygon");
  poly.setAttribute("points", "5,3 19,12 5,21");
  return icon("Play", "icon-play", poly);
}

/** Red stop square. */
export function stopIcon() {
  const rect = document.createElementNS(SVG_NS, "rect");
  rect.setAttribute("x", "5");
  rect.setAttribute("y", "5");
  rect.setAttribute("width", "14");
  rect.setAttribute("height", "14");
  return icon("Stop", "icon-stop", rect);
}
