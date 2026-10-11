// The template side of the kuma contract (see CLAUDE.md next to the templates). Load it after
// gsap.min.js and call `kumaTemplate(defaults)` once.
//
// Works standalone too (no window.kuma): vars fall back to `defaults` and the duration to
// the root's data-duration. Standalone, every var becomes an inline `--var-<name>` custom
// property on <html> (points as `--var-<name>-x` / `-y`) so the CSS sees the defaults;
// under kuma the daemon has already injected the real values as a <style> in <head>.
(function () {
  const k = window.kuma || {};
  const root = document.documentElement;
  window.kumaTemplate = function (defaults) {
    const vars = Object.assign({}, defaults, k.vars || {});
    if (!window.kuma) {
      for (const [name, v] of Object.entries(vars)) {
        if (v && typeof v === "object") for (const [s, x] of Object.entries(v)) root.style.setProperty(`--var-${name}-${s}`, String(x));
        else root.style.setProperty(`--var-${name}`, String(v));
      }
    }
    const comp = document.querySelector("[data-composition-id]");
    let tl = null;
    const api = {
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
        tl = tl || gsap.timeline({ paused: true });
        (window.__timelines = window.__timelines || {}).main = tl;
        return tl;
      },
      /** Call `fn(t)` whenever the timeline renders time `t`, however it is seeked: for
       *  per-frame JS that GSAP does not tween (a canvas, a computed path). Never draw from
       *  `onUpdate` — `tl.seek(t)` suppresses callbacks, and the preview stays stale. */
      onFrame(fn) {
        const length = api.duration;
        const clock = { at: (v) => (v === undefined ? 0 : fn(v)) };
        // A tweened getter/setter is written on every render; `fromTo` draws t = 0 at once.
        api.timeline().fromTo(clock, { at: 0 }, { at: length, duration: length, ease: "none" }, 0);
      },
      /** `onFrame` for a picture that changes only now and then: `fn(t)` runs through each of
       *  `spans` (`[[from, to], …]` in clip seconds, in order) and nowhere else, so the
       *  renderer can reuse the frames between them rather than capture every one, which
       *  `onFrame`'s single tween over the whole clip rules out. Between two spans `fn` must
       *  draw one picture whatever `t` it is given: it is called with either edge. */
      onFrames(spans, fn) {
        for (const [from, to] of spans) {
          // GSAP asks the getter where a tween starts, whatever `fromTo` was told.
          const clock = { at: (v) => (v === undefined ? from : fn(v)) };
          api.timeline().fromTo(clock, { at: from }, { at: to, duration: to - from, ease: "none", immediateRender: false }, from);
        }
        fn(0);
      },
    };
    return api;
  };
})();
