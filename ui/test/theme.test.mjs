// Themes: the palettes in `themes.css` against the list in `theme.ts`.
//
// A theme is written in two places on purpose — colours in CSS, label and note where the
// picker is built — and every way of drifting is silent. A palette with no entry is a theme
// nobody can choose; an entry with no palette switches to the default and reads as a bug in
// the picker; a theme missing a token inherits the default's colour for it, so a Tokyo
// Night editor keeps one orange playhead and only the eye finds out.
//
// Text in, text out: no browser, no daemon, no build. This one never skips.

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import fs from "node:fs";
import path from "node:path";

const SRC = path.join(import.meta.dirname, "..", "src");
const sheet = fs.readFileSync(path.join(SRC, "themes.css"), "utf8");
const ts = fs.readFileSync(path.join(SRC, "theme.ts"), "utf8");

/** Every `[data-theme="…"]` block in the stylesheet, as id → the tokens it declares.
 *  Comments go first: they hold both `data-theme` and `--token` in their prose. */
function palettes() {
  const out = new Map();
  for (const [, selector, body] of sheet.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/([^{}]*)\{([^}]*)\}/g)) {
    const ids = [...selector.matchAll(/\[data-theme="([a-z0-9-]+)"\]/g)].map((m) => m[1]);
    const tokens = new Set([...body.matchAll(/(--[a-z0-9-]+)\s*:/g)].map((m) => m[1]));
    for (const id of ids) out.set(id, tokens);
  }
  return out;
}

/** The ids in `THEMES`, in order — the first is the default. */
const registered = () => {
  const list = ts.slice(ts.indexOf("export const THEMES"), ts.indexOf("const DEFAULT"));
  return [...list.matchAll(/id:\s*"([a-z0-9-]+)"/g)].map((m) => m[1]);
};

describe("themes", () => {
  const css = palettes();
  const ids = registered();

  it("has a palette for every theme offered, and offers every palette", () => {
    assert.ok(ids.length >= 2, "expected the default and at least one other theme");
    assert.deepEqual([...css.keys()].sort(), [...ids].sort());
  });

  it("gives every theme the whole set of tokens", () => {
    // The default is the contract: a token it declares is one the rest of the stylesheet
    // may be using, so every other palette owes a value for it.
    const base = css.get(ids[0]);
    assert.ok(base && base.size > 20, `the default palette (${ids[0]}) looks empty`);
    for (const id of ids.slice(1)) {
      const theirs = css.get(id);
      const missing = [...base].filter((t) => !theirs.has(t));
      const extra = [...theirs].filter((t) => !base.has(t));
      assert.deepEqual(missing, [], `${id} leaves these to the default: ${missing.join(", ")}`);
      assert.deepEqual(extra, [], `${id} declares tokens no other theme has: ${extra.join(", ")}`);
    }
  });

  it("keeps the editor's colours in the palettes, not in the layout", () => {
    // `style.css` names tokens; the literals left in it are the ones that are not palette
    // — the black a video is letterboxed against, the white and black backdrops the
    // workbench offers behind an overlay, white on a coloured chip. Anything new that is
    // a colour belongs in `themes.css`, or a theme cannot move it.
    const layout = fs.readFileSync(path.join(SRC, "style.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    const literals = [...layout.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((m) => m[0].toLowerCase());
    assert.deepEqual([...new Set(literals)].sort(), ["#000", "#fff"]);
  });

  it("never sets a colour scheme in a palette", () => {
    // A `color-scheme: dark` inherited by the overlay iframes stops Chromium compositing
    // them and paints an overlay's white canvas over the video — the bug `composite.test`
    // exists for. Themes are chrome; they do not get to touch that.
    assert.equal(/color-scheme\s*:/.test(sheet), false);
  });
});
