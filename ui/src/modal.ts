// Asking the user for something, in a dialog.
//
// Not `window.prompt`/`confirm`: Electron does not implement them — they throw "prompt()
// is not supported" — so anything built on them works in a browser tab and silently does
// nothing in the app. A `<dialog>` works in both, looks like the rest of the editor, and
// brings modality, focus and Escape with it.

import { button, el } from "./dom";

export interface FormRow { label: string; input: HTMLElement }

/**
 * The shell every dialog here shares: modal, dismissed by Escape and by the backdrop,
 * cleaned up on the way out and settled exactly once. The caller fills `dlg` and decides
 * what else counts as accepting.
 */
function shell(onClose: (ok: boolean) => void): { dlg: HTMLDialogElement; done: (ok: boolean) => void } {
  const dlg = el("dialog", "modal");
  let settled = false;
  const done = (ok: boolean) => {
    if (settled) return;
    settled = true;
    dlg.close();
    dlg.remove();
    onClose(ok);
  };
  dlg.addEventListener("cancel", (e) => { e.preventDefault(); done(false); });
  dlg.addEventListener("click", (e) => { if (e.target === dlg) done(false); });
  document.body.appendChild(dlg);
  return { dlg, done };
}

/** Show something to read — no question, one way out. `className` widens or otherwise
 *  shapes the dialog for the content it is holding. */
export function showInfo(opts: { title: string; body: HTMLElement; className?: string }): void {
  const { dlg, done } = shell(() => {});
  if (opts.className) dlg.classList.add(opts.className);
  const close = button("Close", () => done(false), "", "primary");
  const actions = el("div", "modal-actions");
  actions.appendChild(close);
  dlg.append(el("h3", "", opts.title), opts.body, actions);
  dlg.showModal();
  close.focus();
}

export interface AskFormOptions {
  title: string;
  /** A line under the title: what the choice above it means. */
  note?: string;
  rows: FormRow[];
  /** Label for the confirming button. */
  ok?: string;
}

/**
 * Show `rows` as a modal form. Resolves true when it is submitted and false when it is
 * cancelled; the caller owns the inputs and reads their values itself, so a dialog is
 * built from the same `dom.ts` helpers as the inspector's fields.
 */
export function askForm(opts: AskFormOptions): Promise<boolean> {
  return new Promise((resolve) => {
    const { dlg, done } = shell(resolve);
    const form = el("form");
    form.method = "dialog";

    const actions = el("div", "modal-actions");
    const cancel = button("Cancel", () => done(false));
    cancel.type = "button";
    const ok = button(opts.ok ?? "OK", () => {}, "", "primary");
    ok.type = "submit";
    actions.append(cancel, ok);

    form.appendChild(el("h3", "", opts.title));
    if (opts.note) form.appendChild(el("p", "muted tiny", opts.note));
    for (const r of opts.rows) {
      const row = el("div", "field");
      row.append(el("label", "", r.label), r.input);
      form.appendChild(row);
    }
    form.appendChild(actions);
    dlg.appendChild(form);

    // Submitting is the only path that accepts; Escape and the backdrop cancel, which the
    // shell already does.
    form.onsubmit = (e) => { e.preventDefault(); done(true); };

    dlg.showModal();
    const first = opts.rows[0]?.input;
    if (first instanceof HTMLInputElement) { first.focus(); first.select(); }
    else if (first instanceof HTMLSelectElement) first.focus();
  });
}

export interface AskTextOptions {
  title: string;
  label?: string;
  value?: string;
  placeholder?: string;
  /** Label for the confirming button. */
  ok?: string;
}

/** Resolves with the trimmed text, or null if it was cancelled or left empty. */
export async function askText(opts: AskTextOptions): Promise<string | null> {
  const input = el("input");
  input.type = "text";
  input.value = opts.value ?? "";
  if (opts.placeholder) input.placeholder = opts.placeholder;
  const ok = await askForm({ title: opts.title, rows: [{ label: opts.label ?? "Name", input }], ok: opts.ok ?? "Create" });
  return ok ? input.value.trim() || null : null;
}
