// Centered overlay modals: edit channel and confirm delete.
// Mirror render_edit_popup / render_confirm_delete_popup in renderer.rs.
import { el, box, replace } from "./box.js";

/**
 * Render the edit-channel modal into `container` (a #modal-root).
 * `state` = { index, frequency, name, field: "frequency" | "name" }.
 * `actions` = { onSave, onCancel }.
 * Returns { frequencyInput, nameInput, close, setActive }.
 */
export function openEditModal(container, state, actions) {
  const frequencyInput = el("input", { class: "field freq" });
  frequencyInput.value = state.frequency;
  frequencyInput.placeholder = "118.100";
  frequencyInput.spellcheck = false;

  const nameInput = el("input", { class: "field" });
  nameInput.value = state.name;
  nameInput.maxLength = 16;
  nameInput.spellcheck = false;

  const dialog = box("Edit Channel", {},
    el("div", { class: "modal-field" },
      box("Frequency (MHz)", {}, frequencyInput),
    ),
    el("div", { class: "modal-field" },
      box("Name", {}, nameInput),
    ),
    el("div", { class: "modal-help", text: "Tab: Switch Field | Enter: Save | Esc: Cancel" }),
    el("div", { class: "actions" },
      el("button", { class: "btn", text: "Enter: Save", onclick: actions.onSave }),
      el("button", { class: "btn", text: "Esc: Cancel", onclick: actions.onCancel }),
    ),
  );

  const backdrop = el("div", { class: "modal-backdrop" }, dialog);
  replace(container, backdrop);

  // Highlight the active field like the console does. Driven explicitly
  // (setActive) because focus events are suppressed while the browser
  // window itself is unfocused; the focus/blur listeners cover clicks.
  const highlight = () => {
    const active = document.activeElement;
    for (const input of [frequencyInput, nameInput]) {
      input.closest(".box").classList.toggle("active", input === active);
    }
  };
  for (const input of [frequencyInput, nameInput]) {
    input.addEventListener("focus", highlight);
    input.addEventListener("blur", highlight);
  }
  const setActive = (input) => {
    highlight();
    input.focus();
    highlight();
  };
  setActive(state.field === "name" ? nameInput : frequencyInput);

  const close = () => replace(container);
  return { frequencyInput, nameInput, close, setActive };
}

/**
 * Render the confirm-delete modal.
 * `actions` = { onYes, onNo }. Returns { close }.
 */
export function openConfirmDelete(container, index, actions) {
  const dialog = box("Confirm Delete", { boxClass: "danger" },
    el("div", { class: "modal-text" },
      el("div", { text: `Are you sure you want to delete channel ${index}?` }),
      el("div", { text: "" }),
      el("div", { text: "(y) Yes / (n) No" }),
    ),
    el("div", { class: "actions" },
      el("button", { class: "btn danger", text: "y: Yes", onclick: actions.onYes }),
      el("button", { class: "btn", text: "n: No", onclick: actions.onNo }),
    ),
  );
  const backdrop = el("div", { class: "modal-backdrop" }, dialog);
  replace(container, backdrop);
  return { close: () => replace(container) };
}
