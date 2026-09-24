// The window's own title bar, VS Code style: menu names on the left, the project in the
// middle, window buttons on the right.
//
// Only in the desktop shell, whose editor windows are frameless. In a browser there is
// nothing to draw and `init` does nothing, so the page is unchanged there.
//
// The bar is ours; the menus are not. Clicking a name asks the main process to pop that
// submenu natively, so accelerator labels, separators and platform behaviour are the real
// thing rather than an HTML imitation that drifts from the application menu.

import { el } from "../dom";
import type { DesktopBridge } from "../desktop";

/** Anything interactive needs `no-drag`, or the window moves instead of the click landing. */
function noDrag<T extends HTMLElement>(e: T): T {
  e.style.setProperty("-webkit-app-region", "no-drag");
  return e;
}

export function initTitlebar(app: DesktopBridge, title: string): void {
  const host = document.getElementById("titlebar");
  if (!host) return;
  document.body.classList.add("has-titlebar");

  const menu = el("div", "tb-menu");
  const label = el("div", "tb-title", title);
  const controls = el("div", "tb-controls");
  host.append(menu, label, controls);

  // One button open at a time, and the native popup closes itself, so the only state to
  // keep is which button should look pressed while its menu is down.
  let open: HTMLButtonElement | null = null;
  const clear = () => {
    open?.classList.remove("open");
    open = null;
  };

  const renderMenu = (labels: string[]) => {
    menu.replaceChildren();
    clear();
    for (const name of labels) {
      const b = noDrag(el("button", "tb-item"));
      b.textContent = name;
      b.onclick = async () => {
        // A second click on the same name is a close, matching the native bar.
        if (open === b) return clear();
        clear();
        open = b;
        b.classList.add("open");
        const r = b.getBoundingClientRect();
        try {
          await app.popupMenu(name, r.left, r.bottom);
        } finally {
          // `popupMenu` resolves when the menu closes, whichever way it closed.
          if (open === b) clear();
        }
      };
      menu.appendChild(b);
    }
  };

  const button = (cls: string, path: string, title: string, action: "minimize" | "maximize" | "close") => {
    const b = noDrag(el("button", `tb-btn ${cls}`));
    b.title = title;
    b.setAttribute("aria-label", title);
    // Inline SVG rather than glyphs: the box-drawing characters these usually use are
    // not in every font and fall back to tofu. `path` is a literal defined below, never
    // anything from the project or an overlay, so this is not the `innerHTML` that
    // `dom.ts` warns about.
    b.innerHTML = `<svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true">${path}</svg>`;
    b.onclick = () => void app.windowControl(action);
    return b;
  };

  const min = button("min", `<path d="M1.5 6h9" stroke="currentColor" stroke-width="1" fill="none"/>`, "Minimize", "minimize");
  const max = button("max", `<rect x="2" y="2" width="8" height="8" stroke="currentColor" stroke-width="1" fill="none"/>`, "Maximize", "maximize");
  const close = button("close", `<path d="M2.5 2.5l7 7M9.5 2.5l-7 7" stroke="currentColor" stroke-width="1.1" fill="none"/>`, "Close", "close");
  controls.append(min, max, close);

  const restoreIcon = `<path d="M3.5 4.5V2.5h6v6h-2" stroke="currentColor" stroke-width="1" fill="none"/><rect x="1.5" y="4.5" width="6" height="6" stroke="currentColor" stroke-width="1" fill="none"/>`;
  const maximizeIcon = `<rect x="2" y="2" width="8" height="8" stroke="currentColor" stroke-width="1" fill="none"/>`;
  const applyState = (s: { maximized: boolean }) => {
    max.innerHTML = `<svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true">${s.maximized ? restoreIcon : maximizeIcon}</svg>`;
    max.title = s.maximized ? "Restore" : "Maximize";
    document.body.classList.toggle("maximized", s.maximized);
  };

  // Double-clicking the empty part of the bar toggles maximize, as a real title bar does.
  host.ondblclick = (e) => {
    if ((e.target as HTMLElement).closest("button")) return;
    void app.windowControl("maximize");
  };

  void app.menuLabels().then(renderMenu);
  app.onMenuChanged(renderMenu);
  void app.windowState().then(applyState);
  app.onWindowState(applyState);
}
