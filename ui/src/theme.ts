// Which palette the editor wears.
//
// A theme is a block of tokens in `themes.css` and an entry in the list below; applying
// one is a `data-theme` on the root, so there is no second stylesheet to load and nothing
// to rebuild. The choice is *this browser's*, not the project's: it is remembered in
// `localStorage` beside the transcribe language, never patched into the document. A theme
// is how the editor looks to you, and two people opening the same file may want different
// answers — which is also why it is nowhere near the daemon.
//
// One consequence, worth knowing rather than working around: storage is per origin, and a
// daemon's origin is its port, so a project that opens on 7456 starts at the default even
// though the one on 7455 is themed. The transcribe language has always had this. The shell
// could carry the choice across its windows through the bridge, and that is the change to
// make if it ever becomes annoying — not a value in the project file.
//
// What a theme is not, and what keeps this file small: it dresses the editor's chrome and
// stops there. Overlays, bakes and renders never see these tokens, so the video looks the
// same whichever theme is on. Styling a video is what a design system is for
// (`kuma design-system`), and that is a document you edit, not a cascade.

import { Cmd, command } from "./commands";
import { selectInput } from "./dom";
import { askForm } from "./modal";

export interface Theme {
  /** The `data-theme` value, and what is stored. */
  id: string;
  label: string;
  /** One line, shown as the option's tooltip. */
  note: string;
}

/** The themes offered, in the order the picker lists them. The first is the default —
 *  what an editor that has never been themed, or that was pointed at a theme that no
 *  longer exists, falls back to. Every id here needs a block in `themes.css`; the test
 *  holds the two together. */
export const THEMES: Theme[] = [
  { id: "kuma", label: "Kuma", note: "The default: warm orange on charcoal, and nothing on screen brighter than the picture." },
  { id: "tokyo-night", label: "Tokyo Night", note: "Retro neon on deep indigo — hot pink playhead, violet overlays, and a glow where you look for the time." },
];

const DEFAULT = THEMES[0].id;
const KEY = "kuma.theme";

const known = (id: string | null | undefined): string => (THEMES.some((t) => t.id === id) ? id! : DEFAULT);

/** The theme in force, which is whatever the root says it is. */
export const currentTheme = (): string => known(document.documentElement.dataset.theme);

/** Put a theme on, without remembering it — what the picker does while you look at it. */
function wear(id: string): void {
  document.documentElement.dataset.theme = known(id);
}

/** Put a theme on and keep it. A private window has no storage and throws on access,
 *  which must not cost the theme itself. */
export function setTheme(id: string): void {
  wear(id);
  try { localStorage.setItem(KEY, known(id)); } catch { /* no storage: this session only */ }
}

/**
 * Wear the remembered theme. Called before the panels mount — the document has already
 * painted the default palette by then, so anything later is a flash of the wrong colours.
 */
export function initTheme(): void {
  let saved: string | null = null;
  try { saved = localStorage.getItem(KEY); } catch { /* no storage: the default, then */ }
  wear(known(saved));
}

/** Pick a theme: the list, applied as you move through it so the choice is the thing
 *  itself rather than a name, and put back if the dialog is cancelled. */
export async function chooseTheme(): Promise<void> {
  const before = currentTheme();
  const input = selectInput(
    THEMES.map((t) => ({ value: t.id, label: t.label, title: t.note })),
    before,
    (id) => wear(id),
  );
  const ok = await askForm({
    title: "Theme",
    note: "The editor only. What a theme changes is this window — never a template, a bake or a render, so the video looks the same whichever one is on.",
    rows: [{ label: "Theme", input }],
    ok: "Use",
  });
  if (ok) setTheme(input.value);
  else wear(before);
}

export function installTheme(): void {
  initTheme();
  command(Cmd.theme, "Theme…", () => chooseTheme());
}
