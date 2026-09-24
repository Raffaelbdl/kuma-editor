// Drawn overlays: a <canvas> painted as a pure function of time, driven by the one GSAP
// timeline the renderer seeks. Load it after gsap.min.js and kuma.js (and rough.min.js
// for the hand-drawn look); it adds `kumaCanvas` to `window`.
//
//   const { vars, width, height, duration, timeline } = kumaTemplate({ … });
//   const tl = timeline();
//   kumaCanvas(tl, "#paint", (g, t) => {
//     const r = g.rough("hill");                       // a Rough.js canvas, seeded for this frame
//     r.circle(g.w / 2, g.h / 2, 30 * g.u, { fill: vars.accent, fillStyle: "solid" });
//   }, { width, height, duration, size: vars.size });
//
// Every frame is drawn from `t` alone: the canvas is cleared first, nothing carries over,
// and `Math.random` is a generator seeded by the frame while `draw` runs, so libraries
// that call it (Rough.js does) draw the same frame every time. Wobble changes BOIL times a
// second, like a hand-drawn line redrawn on every drawing: `g.rand(key)` and `g.rough(key)`
// keep one value per key until the next drawing, so a still thing stays still between them.
//
// The motion helpers (spring, jump, take, arcPt, kf, onTwos) are adapted from
// ClaudeAnimationBase, MIT, © 2026 John Heibel.
(function () {
  const BOIL = 12;

  // ---------- numbers ----------
  const clamp = (x, a = 0, b = 1) => Math.max(a, Math.min(b, x));
  const lerp = (a, b, k) => a + (b - a) * k;
  /** 0..1 progress of t through [a, b]. */
  const seg = (t, a, b) => clamp((t - a) / (b - a));
  const ease = (x) => { x = clamp(x); return x * x * (3 - 2 * x); };
  const easeIn = (x) => Math.pow(clamp(x), 3);
  const easeOut = (x) => 1 - Math.pow(1 - clamp(x), 3);
  const backOut = (x) => { x = clamp(x); const s = 1.9; return 1 + (s + 1) * Math.pow(x - 1, 3) + s * Math.pow(x - 1, 2); };

  /** A stable 32-bit hash of any string or number. */
  function hash32(key) {
    let h = 2166136261;
    for (const c of String(key)) h = Math.imul(h ^ c.charCodeAt(0), 16777619);
    return h >>> 0;
  }
  /** A stable value in [0, 1) for a key: the same on every frame and every bake. */
  const hash = (key) => hash32(key) / 4294967296;
  /** A seeded generator of values in [0, 1). */
  function prng(seed) {
    let s = seed >>> 0 || 1;
    return () => { s = (s + 0x6d2b79f5) >>> 0; let x = Math.imul(s ^ (s >>> 15), 1 | s); x ^= x + Math.imul(x ^ (x >>> 7), 61 | x); return ((x ^ (x >>> 14)) >>> 0) / 4294967296; };
  }
  /** Smooth value noise in about [0, 1], for drift, shimmer and terrain. */
  function noise(x, y = 0, z = 0) {
    const xi = Math.floor(x), yi = Math.floor(y), zi = Math.floor(z);
    const fx = ease(x - xi), fy = ease(y - yi), fz = ease(z - zi);
    const v = (i, j, k) => hash(`${xi + i},${yi + j},${zi + k}`);
    const plane = (k) => lerp(lerp(v(0, 0, k), v(1, 0, k), fx), lerp(v(0, 1, k), v(1, 1, k), fx), fy);
    return lerp(plane(0), plane(1), fz);
  }

  // ---------- motion, as pure functions of t ----------
  /** Keyframes: kf(t, [[t0, v0], [t1, v1], …], easeFn). Values may be arrays of numbers. */
  function kf(t, keys, e = ease) {
    if (t <= keys[0][0]) return keys[0][1];
    for (let i = 1; i < keys.length; i++) {
      if (t < keys[i][0]) {
        const [a, va] = keys[i - 1], [b, vb] = keys[i], k = e((t - a) / (b - a));
        return Array.isArray(va) ? va.map((v, j) => lerp(v, vb[j], k)) : lerp(va, vb, k);
      }
    }
    return keys[keys.length - 1][1];
  }
  /** A damped wobble kicked at t0: 0 before, dying away after. For settles and follow-through. */
  const spring = (t, t0, k = 6, w = 18) => (t < t0 ? 0 : Math.exp(-k * (t - t0)) * Math.sin(w * (t - t0)));
  /** One spring kick per event time. */
  const ring = (t, times, k = 6, w = 18) => times.reduce((s, t0) => s + spring(t, t0, k, w), 0);
  /** Hold each drawing for two frames at 24 fps, like hand-drawn animation "on twos". */
  const onTwos = (t) => Math.floor(t * 12 + 1e-6) / 12;
  /** A point on a thrown arc from p0 to p1 peaking h above the straight line; k = 0..1 along it. */
  const arcPt = (p0, p1, h, k) => [lerp(p0[0], p1[0], k), lerp(p0[1], p1[1], k) - h * 4 * k * (1 - k)];
  /** A hop from t0 to t1, h high: crouch, stretch, arc, squash on landing. `y` up is negative; `sq` > 0 squashes. */
  function jump(t, t0, t1, h = 1) {
    if (t < t0 - 0.12) return { y: 0, sq: 0 };
    if (t < t0) return { y: 0, sq: 0.18 * ease(seg(t, t0 - 0.12, t0)) };
    if (t < t1) { const k = (t - t0) / (t1 - t0); return { y: -h * 4 * k * (1 - k), sq: -0.16 * Math.abs(1 - 2 * k) }; }
    const a = t - t1;
    return { y: 0, sq: 0.22 * Math.exp(-8 * a) * Math.cos(20 * a) };
  }
  /** A surprise take peaking at t0: a quick squash, then a stretch up that springs back. */
  function take(t, t0, amt = 1) {
    if (t < t0 - 0.1) return { y: 0, sq: 0 };
    if (t < t0) return { y: 0, sq: 0.12 * amt * ease(seg(t, t0 - 0.1, t0)) };
    const a = t - t0;
    return { y: -amt * Math.exp(-7 * a) * Math.max(0, Math.cos(9 * a)), sq: -0.26 * amt * Math.exp(-6 * a) * Math.cos(16 * a) };
  }

  // ---------- the canvas ----------
  // The Rough.js calls that draw; each gets a `seed` for its key and the current drawing.
  const DRAWS = new Set(["line", "rectangle", "ellipse", "circle", "linearPath", "polygon", "arc", "curve", "path"]);

  function kumaCanvas(tl, canvas, draw, opts = {}) {
    const el = typeof canvas === "string" ? document.querySelector(canvas) : canvas;
    const k = window.kuma || {};
    const w = (el.width = opts.width || k.width || 1920);
    const h = (el.height = opts.height || k.height || 1080);
    const ctx = el.getContext("2d");
    const length = opts.duration || k.duration || 4;
    const boil = opts.boil || BOIL;
    const rc = window.rough && window.rough.canvas(el);

    function render(t) {
      const drawing = Math.floor(t * boil + 1e-6);
      const seed = (key) => hash32(`${key}|${drawing}`) || 1;
      const g = {
        ctx, w, h, t, drawing,
        /** 1% of the frame height, times `size`: the same unit as CSS's `--u`. */
        u: (h / 100) * (opts.size || 1),
        /** A value in [0, 1) for `key` that holds until the next drawing. */
        rand: (key) => hash(`${key}|${drawing}`),
        /** A Rough.js canvas whose strokes are seeded by `key` and the drawing. */
        rough(key) {
          if (!rc) throw new Error("kumaCanvas: load _vendor/rough.min.js before g.rough()");
          return new Proxy(rc, {
            get(target, name) {
              const f = target[name];
              if (!DRAWS.has(name)) return typeof f === "function" ? f.bind(target) : f;
              return (...args) => {
                const last = args[args.length - 1];
                const given = last && typeof last === "object" && !Array.isArray(last);
                const o = Object.assign({}, given ? last : {}, { seed: seed(key) });
                return f.apply(target, given ? [...args.slice(0, -1), o] : [...args, o]);
              };
            },
          });
        },
      };
      const real = Math.random;
      Math.random = prng(seed("frame"));
      try {
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.clearRect(0, 0, w, h);
        draw(g, t);
      } finally {
        Math.random = real;
      }
    }

    // One tween across the clip whose property is the clock: GSAP writes it on every render
    // of the timeline, however it is seeked. An `onUpdate` would miss seeks that suppress
    // events (`tl.seek(t)` does by default) and leave a stale frame.
    let at = 0;
    const clock = { at: (v) => (v === undefined ? at : render((at = v))) };
    tl.fromTo(clock, { at: 0 }, { at: length, duration: length, ease: "none" }, 0);
    render(0);
    return render;
  }

  Object.assign(kumaCanvas, { clamp, lerp, seg, ease, easeIn, easeOut, backOut, hash, noise, kf, spring, ring, onTwos, arcPt, jump, take });
  window.kumaCanvas = kumaCanvas;
})();
