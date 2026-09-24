// Entrances and exits by name, for overlays whose motion is a var. Load it after
// gsap.min.js; it adds `kumaMotion` to `window`.
//
//   const tl = timeline();
//   const inEnd = kumaMotion.enter(tl, card, vars.enter, { at: 0, duration: vars.enter_duration, text });
//   kumaMotion.exit(tl, card, vars.exit, { at: duration - vars.exit_duration, duration: vars.exit_duration, after: inEnd });
//
// The names are what a manifest lists as enum options, so every classic blueprint offers
// the same set and means the same thing by each:
//
//   ENTER: none fade slide-up slide-down slide-left slide-right zoom typewriter word-by-word
//   EXIT:  none fade slide-up slide-down slide-left slide-right zoom
//
// A slide names the direction of travel ("slide-up" arrives from below, and leaves
// upwards). Distances are in the element's own font size, so they read the same at any
// size and in either orientation. Curves follow the house numbers in CLAUDE.md: out-curves
// in, in-curves out, scale from 0.9 and never from 0.
(function () {
  const em = (el) => parseFloat(getComputedStyle(el).fontSize) || 16;

  /** The offset a slide travels, in px: along y for up/down, x for left/right. */
  function travel(el, kind, leaving) {
    const d = em(el);
    const sign = leaving ? -1 : 1;
    switch (kind) {
      case "slide-up": return { y: 0.5 * d * sign };
      case "slide-down": return { y: -0.5 * d * sign };
      case "slide-left": return { x: 1.2 * d * sign };
      case "slide-right": return { x: -1.2 * d * sign };
      default: return {};
    }
  }

  /**
   * Replace an element's text with one span per word, or per character, and return them.
   * Spaces stay text nodes, so lines break where they did. A character is a span inside
   * its word's span, which does not wrap: a box per character would let a line break
   * between any two of them, in the middle of a word.
   */
  function split(el, by) {
    const text = el.textContent || "";
    el.textContent = "";
    const out = [];
    for (const part of text.split(/(\s+)/)) {
      if (!part) continue;
      if (/^\s+$/.test(part)) { el.appendChild(document.createTextNode(part)); continue; }
      const word = document.createElement("span");
      word.style.display = "inline-block";
      word.style.whiteSpace = "nowrap";
      el.appendChild(word);
      if (by === "words") { word.textContent = part; out.push(word); continue; }
      for (const ch of Array.from(part)) {
        const c = document.createElement("span");
        c.textContent = ch;
        word.appendChild(c);
        out.push(c);
      }
    }
    return out;
  }

  /**
   * Add `el`'s entrance to `tl` and return the time it settles. `text` is the element
   * holding the words, for the two entrances that work on them (the whole of `el`
   * otherwise); `el` itself fades in quickly first, so a box behind the words is there
   * before they are typed into it.
   */
  function enter(tl, el, kind, o) {
    const at = o.at || 0;
    const duration = Math.max(0.05, o.duration || 0.5);
    if (!kind || kind === "none") return at;
    if (kind === "typewriter" || kind === "word-by-word") {
      const target = o.text || el;
      const parts = split(target, kind === "typewriter" ? "chars" : "words");
      tl.from(el, { opacity: 0, duration: Math.min(0.2, duration / 3), ease: "power1.out" }, at);
      if (!parts.length) return at + duration;
      const step = duration / parts.length;
      if (kind === "typewriter") {
        // A key press is instant: each character is off, then on.
        tl.from(parts, { opacity: 0, duration: 0.001, stagger: step, ease: "none" }, at);
      } else {
        const each = Math.min(0.4, duration);
        const gap = parts.length > 1 ? (duration - each) / (parts.length - 1) : 0;
        tl.from(parts, { opacity: 0, y: 0.3 * em(target), duration: each, stagger: Math.max(0, gap), ease: "power3.out" }, at);
      }
      return at + duration;
    }
    const from = { opacity: 0, duration, ease: "power3.out", ...travel(el, kind, false) };
    if (kind === "zoom") from.scale = 0.9;
    tl.from(el, from, at);
    return at + duration;
  }

  /**
   * Add `el`'s exit to `tl`, starting at `at` but never before `after` (the end of the
   * entrance), so a clip too short for both still shows the thing it is for.
   */
  function exit(tl, el, kind, o) {
    if (!kind || kind === "none") return;
    const duration = Math.max(0.05, o.duration || 0.4);
    const at = Math.max(o.after || 0, o.at || 0);
    const to = { opacity: 0, duration, ease: "power2.in", ...travel(el, kind, true) };
    if (kind === "zoom") to.scale = 1.05;
    tl.to(el, to, at);
  }

  window.kumaMotion = { enter, exit, split };
})();
