// A small popup menu, for the places the page offers a list of actions at the pointer:
// the bin's New button and its right-click.
//
// HTML rather than the shell's native menus, which only the desktop app has: this works
// the same in a browser tab. It closes on a choice, a click anywhere else, Escape, or the
// window losing focus, and only one is ever open.

import { el } from "./dom";

/** `keys` is the shortcut that does the same, shown at the right as a reminder. */
export type MenuItem = { label: string; run: () => void; title?: string; disabled?: boolean; keys?: string } | "separator" | { heading: string };

let open: (() => void) | null = null;

/** Show `items` with the menu's top-left at (x, y), kept inside the window. */
export function showMenu(items: MenuItem[], x: number, y: number): void {
  open?.();
  const menu = el("div", "popup-menu");
  menu.setAttribute("role", "menu");
  const buttons: HTMLButtonElement[] = [];
  for (const item of items) {
    if (item === "separator") { menu.appendChild(el("div", "popup-sep")); continue; }
    if ("heading" in item) { menu.appendChild(el("div", "popup-heading", item.heading)); continue; }
    const b = el("button", "popup-item", item.label);
    if (item.keys) b.appendChild(el("kbd", "", item.keys));
    b.type = "button";
    b.setAttribute("role", "menuitem");
    if (item.title) b.title = item.title;
    b.disabled = !!item.disabled;
    b.onclick = () => { close(); item.run(); };
    menu.appendChild(b);
    buttons.push(b);
  }
  document.body.appendChild(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(4, Math.min(x, innerWidth - r.width - 4))}px`;
  menu.style.top = `${Math.max(4, Math.min(y, innerHeight - r.height - 4))}px`;

  const outside = (e: PointerEvent) => { if (!menu.contains(e.target as Node)) close(); };
  const keys = (e: KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); close(); return; }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const live = buttons.filter((b) => !b.disabled);
    const i = live.indexOf(document.activeElement as HTMLButtonElement);
    live[(i + (e.key === "ArrowDown" ? 1 : live.length - 1)) % live.length]?.focus();
  };
  const close = () => {
    menu.remove();
    removeEventListener("pointerdown", outside, true);
    removeEventListener("keydown", keys, true);
    removeEventListener("blur", close);
    if (open === close) open = null;
  };
  addEventListener("pointerdown", outside, true);
  addEventListener("keydown", keys, true);
  addEventListener("blur", close);
  open = close;
  buttons.find((b) => !b.disabled)?.focus();
}

/** Show `items` under `anchor`, as a button's menu. */
export function menuBelow(anchor: HTMLElement, items: MenuItem[]): void {
  const r = anchor.getBoundingClientRect();
  showMenu(items, r.left, r.bottom + 2);
}
