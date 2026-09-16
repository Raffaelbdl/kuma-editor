// Asking the user for a line of text.
//
// Not `window.prompt`: Electron does not implement it — it throws "prompt() is not
// supported" — so anything built on it works in a browser tab and silently does nothing
// in the app. A `<dialog>` works in both, looks like the rest of the editor, and brings
// modality, focus and Escape with it.

import { button, el } from "./dom";

export interface AskTextOptions {
  title: string;
  label?: string;
  value?: string;
  placeholder?: string;
  /** Label for the confirming button. */
  ok?: string;
}

/** Resolves with the trimmed text, or null if it was cancelled or left empty. */
export function askText(opts: AskTextOptions): Promise<string | null> {
  return new Promise((resolve) => {
    const dlg = el("dialog", "modal");
    const form = el("form");
    form.method = "dialog";

    const input = el("input");
    input.type = "text";
    input.value = opts.value ?? "";
    if (opts.placeholder) input.placeholder = opts.placeholder;

    const row = el("div", "field");
    row.append(el("label", "", opts.label ?? "Name"), input);

    const actions = el("div", "modal-actions");
    const cancel = button("Cancel", () => { done(null); });
    cancel.type = "button";
    const ok = button(opts.ok ?? "Create", () => {});
    ok.type = "submit";
    ok.className = "primary";
    actions.append(cancel, ok);

    form.append(el("h3", "", opts.title), row, actions);
    dlg.appendChild(form);
    document.body.appendChild(dlg);

    let settled = false;
    const done = (v: string | null) => {
      if (settled) return;
      settled = true;
      dlg.close();
      dlg.remove();
      resolve(v);
    };
    // Submitting is the only path that reads the field; Escape and the backdrop cancel.
    form.onsubmit = (e) => {
      e.preventDefault();
      done(input.value.trim() || null);
    };
    dlg.addEventListener("cancel", (e) => { e.preventDefault(); done(null); });
    dlg.addEventListener("click", (e) => { if (e.target === dlg) done(null); });

    dlg.showModal();
    input.focus();
    input.select();
  });
}
