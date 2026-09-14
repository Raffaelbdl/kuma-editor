// The template side of the kuma contract (see overlays/CLAUDE.md). Load it after
// gsap.min.js and call `kumaTemplate(defaults)` once.
//
// Works standalone too (no window.kuma): vars fall back to `defaults` and the duration to
// the root's data-duration. Every var becomes an inline `--var-<name>` custom property on
// <html> (points as `--var-<name>-x` / `-y`), which beats any :root rule in the template's
// own CSS.
(function () {
  const k = window.kuma || {};
  const root = document.documentElement;
  window.kumaTemplate = function (defaults) {
    const vars = Object.assign({}, defaults, k.vars || {});
    for (const [name, v] of Object.entries(vars)) {
      if (v && typeof v === "object") for (const [s, x] of Object.entries(v)) root.style.setProperty(`--var-${name}-${s}`, String(x));
      else root.style.setProperty(`--var-${name}`, String(v));
    }
    const comp = document.querySelector("[data-composition-id]");
    return {
      vars,
      width: k.width || 1920,
      height: k.height || 1080,
      /** Clip length in seconds; the out-animation must be anchored to it. */
      duration: k.duration || Number(comp && comp.dataset.duration) || 4,
      /** Set an element's text; an empty value hides it. */
      text(id, value) {
        const e = document.getElementById(id);
        e.textContent = value;
        e.style.display = value ? "" : "none";
        return e;
      },
      /** The one paused GSAP timeline that the renderer and the preview seek. */
      timeline() {
        const tl = gsap.timeline({ paused: true });
        (window.__timelines = window.__timelines || {}).main = tl;
        return tl;
      },
    };
  };
})();
