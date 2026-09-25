// Drawn overlays: a <canvas> painted as a pure function of time. Load it after gsap.min.js
// and kuma.js (and rough.min.js for the hand-drawn look); it adds `kumaCanvas` to `window`.
// Usage and rules: "Drawn overlays" in library/blueprints/CLAUDE.md.
//
// The motion helpers (spring, jump, take, arcPt, kf, onTwos) are adapted from
// ClaudeAnimationBase, MIT, © 2026 John Heibel.
(function () {
  /** Drawings per second: how often hand-drawn wobble changes ("boil"). */
  const BOIL = 12;

  const clamp = (x, a = 0, b = 1) => Math.max(a, Math.min(b, x));
  const lerp = (a, b, k) => a + (b - a) * k;
  const smooth = (x) => { x = clamp(x); return x * x * (3 - 2 * x); };

  /** A stable 32-bit hash of a string or number. */
  function hash32(key) {
    const s = String(key);
    let h = 2166136261;
    for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
    return h >>> 0;
  }
  /** Murmur3's finaliser: every input bit moves every output bit. */
  function mix32(h) {
    h = Math.imul(h ^ (h >>> 16), 0x85ebca6b);
    h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
    return (h ^ (h >>> 16)) >>> 0;
  }
  /** A stable value in [0, 1) for a key: the same on every frame and every bake, and
   *  unrelated to any other key's, however alike the keys are (`s1`, `s2`, …). */
  const rand = (key) => mix32(hash32(key)) / 4294967296;
  /** The first `rand`, unmixed: keys that differ only at the end give related values, so a
   *  star field placed with it falls into lines. Kept because `_vendor/` is shared by
   *  every project, and changing it would re-draw overlays that already use it. */
  const hash = (key) => hash32(key) / 4294967296;
  /** A seeded generator of values in [0, 1). */
  function prng(seed) {
    let s = seed >>> 0 || 1;
    return () => { s = (s + 0x6d2b79f5) >>> 0; let x = Math.imul(s ^ (s >>> 15), 1 | s); x ^= x + Math.imul(x ^ (x >>> 7), 61 | x); return ((x ^ (x >>> 14)) >>> 0) / 4294967296; };
  }
  /** `fn()` with Math.random seeded: libraries that call it (Rough.js does, even when
   *  seeded) then draw the same on every frame and every bake. */
  function withRandom(seed, fn) {
    const real = Math.random;
    Math.random = prng(seed);
    try {
      return fn();
    } finally {
      Math.random = real;
    }
  }
  /** A lattice point's value in [0, 1), from integers alone: no strings, no allocation. */
  function lattice(x, y, z) {
    let h = Math.imul(x, 0x27d4eb2d) ^ Math.imul(y, 0x165667b1) ^ Math.imul(z, 0x9e3779b1);
    h = Math.imul(h ^ (h >>> 15), 0x85ebca6b);
    h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
    return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
  }
  /** Smooth value noise in [0, 1], for drift, shimmer and terrain. */
  function noise(x, y = 0, z = 0) {
    const xi = Math.floor(x), yi = Math.floor(y), zi = Math.floor(z);
    const fx = smooth(x - xi), fy = smooth(y - yi), fz = smooth(z - zi);
    const plane = (k) => lerp(
      lerp(lattice(xi, yi, zi + k), lattice(xi + 1, yi, zi + k), fx),
      lerp(lattice(xi, yi + 1, zi + k), lattice(xi + 1, yi + 1, zi + k), fx), fy);
    return lerp(plane(0), plane(1), fz);
  }

  // ---------- loops: motion that ends where it began, at any clip length ----------
  /** How many whole cycles of about `period` seconds fit in `duration`: at least one. */
  const cycles = (duration, period) => Math.max(1, Math.round(duration / period));
  /** Where `t` is in its cycle, in [0, 1), when `n` cycles span `duration`. */
  const phase = (t, duration, n = 1) => { const p = (t / duration) * n; return p - Math.floor(p); };
  /** Smooth noise in [0, 1] at (x, y) that returns to where it began after `duration`,
   *  `n` times over: it walks a circle through the noise field instead of a line. */
  function loopNoise(x, y, t, duration, n = 1, radius = 0.5) {
    const a = 2 * Math.PI * phase(t, duration, n);
    return noise(x, y + radius * Math.cos(a), radius * Math.sin(a));
  }

  // ---------- colour ----------
  /** `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` as [r, g, b, a], a in [0, 1]. */
  function parseHex(hex) {
    let h = String(hex).replace("#", "");
    if (h.length <= 4) h = h.split("").map((c) => c + c).join("");
    const n = (i) => parseInt(h.slice(i, i + 2), 16);
    return [n(0), n(2), n(4), h.length >= 8 ? n(6) / 255 : 1];
  }
  const css = ([r, g, b, a]) => `rgba(${Math.round(r)}, ${Math.round(g)}, ${Math.round(b)}, ${+a.toFixed(4)})`;
  /** A hex colour at opacity `a` (times its own), as a CSS `rgba(…)`. */
  const rgba = (hex, a = 1) => { const c = parseHex(hex); c[3] *= a; return css(c); };
  /** The colour `k` of the way from hex `a` to hex `b` (0 is `a`), as a CSS `rgba(…)`. */
  const mix = (a, b, k) => { const q = parseHex(b); return css(parseHex(a).map((v, i) => lerp(v, q[i], clamp(k)))); };

  // ---------- motion, as pure functions of t (eases: gsap.parseEase("power3.out") etc.) ----------
  /** Keyframes: kf(t, [[t0, v0], [t1, v1], …], ease). Values may be arrays of numbers. */
  function kf(t, keys, ease = smooth) {
    if (t <= keys[0][0]) return keys[0][1];
    for (let i = 1; i < keys.length; i++) {
      if (t < keys[i][0]) {
        const [a, va] = keys[i - 1], [b, vb] = keys[i], k = ease((t - a) / (b - a));
        return Array.isArray(va) ? va.map((v, j) => lerp(v, vb[j], k)) : lerp(va, vb, k);
      }
    }
    return keys[keys.length - 1][1];
  }
  /** A damped wobble kicked at t0: 0 before, dying away after. For settles and follow-through. */
  const spring = (t, t0, k = 6, w = 18) => (t < t0 ? 0 : Math.exp(-k * (t - t0)) * Math.sin(w * (t - t0)));
  /** One spring kick per event time. */
  const ring = (t, times, k = 6, w = 18) => times.reduce((s, t0) => s + spring(t, t0, k, w), 0);
  /** Hold each drawing for one boil step, like hand-drawn animation "on twos". */
  const onTwos = (t) => Math.floor(t * BOIL + 1e-6) / BOIL;
  /** A point on a thrown arc from p0 to p1 peaking h above the straight line; k = 0..1 along it. */
  const arcPt = (p0, p1, h, k) => [lerp(p0[0], p1[0], k), lerp(p0[1], p1[1], k) - h * 4 * k * (1 - k)];
  /** A hop from t0 to t1, h high: crouch, stretch, arc, squash on landing. `y` up is negative; `sq` > 0 squashes. */
  function jump(t, t0, t1, h = 1) {
    if (t < t0 - 0.12) return { y: 0, sq: 0 };
    if (t < t0) return { y: 0, sq: 0.18 * smooth((t - t0 + 0.12) / 0.12) };
    if (t < t1) { const k = (t - t0) / (t1 - t0); return { y: -h * 4 * k * (1 - k), sq: -0.16 * Math.abs(1 - 2 * k) }; }
    const a = t - t1;
    return { y: 0, sq: 0.22 * Math.exp(-8 * a) * Math.cos(20 * a) };
  }
  /** A surprise take peaking at t0: a quick squash, then a stretch up that springs back. */
  function take(t, t0, amt = 1) {
    if (t < t0 - 0.1) return { y: 0, sq: 0 };
    if (t < t0) return { y: 0, sq: 0.12 * amt * smooth((t - t0 + 0.1) / 0.1) };
    const a = t - t0;
    return { y: -amt * Math.exp(-7 * a) * Math.max(0, Math.cos(9 * a)), sq: -0.26 * amt * Math.exp(-6 * a) * Math.cos(16 * a) };
  }

  /** Paint `canvas` from `draw(g, t)` on every frame of `tpl`, the `kumaTemplate(…)` result. */
  function kumaCanvas(tpl, canvas, draw) {
    const el = typeof canvas === "string" ? document.querySelector(canvas) : canvas;
    const w = (el.width = tpl.width), h = (el.height = tpl.height);
    const ctx = el.getContext("2d");
    const u = (h / 100) * (tpl.vars.size || 1);
    // Stills outlive the frame that painted them, so they are seeded by their key alone:
    // which frame asks first (the bake starts at 0, the preview anywhere) must not matter.
    // Each is a frame-sized canvas kept for the page's life, so keys are constants.
    const stills = new Map();
    const still = (key, paint) => {
      if (!stills.has(key)) {
        const c = document.createElement("canvas");
        c.width = w;
        c.height = h;
        const seed = mix32(hash32(`still|${key}`)) || 1;
        withRandom(seed, () => paint({ ctx: c.getContext("2d"), w, h, u, rough: () => window.rough.canvas(c, { options: { seed } }) }));
        stills.set(key, c);
      }
      return stills.get(key);
    };
    tpl.onFrame((t) => {
      const drawing = Math.floor(t * BOIL + 1e-6);
      const seed = (key) => hash32(`${key}|${drawing}`) || 1;
      const g = {
        ctx, w, h, u,
        /** A seed for `key` that holds until the next drawing, for any seedable library. */
        seed,
        /** A value in [0, 1) for `key` that holds until the next drawing: wobble. */
        jitter: (key) => mix32(seed(key)) / 4294967296,
        /** The first `jitter`, unmixed, kept for the overlays that use it (see `hash`). */
        rand: (key) => seed(key) / 4294967296,
        /** A Rough.js canvas whose every stroke is seeded by `key` and the drawing. */
        rough: (key) => window.rough.canvas(el, { options: { seed: seed(key) } }),
        /** A frame-sized canvas painted once by `paint({ ctx, w, h, u, rough })` and kept:
         *  the part of a picture that never moves, drawn with `ctx.drawImage(…, 0, 0)`.
         *  One per key for the life of the page, so a key is a name, never a time. */
        still,
      };
      ctx.save();
      try {
        ctx.clearRect(0, 0, w, h);
        withRandom(seed("frame"), () => draw(g, t));
      } finally {
        ctx.restore();
      }
    });
  }

  Object.assign(kumaCanvas, { rand, hash, noise, cycles, phase, loopNoise, rgba, mix, kf, spring, ring, onTwos, arcPt, jump, take });
  window.kumaCanvas = kumaCanvas;
})();
