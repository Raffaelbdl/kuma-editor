// `_vendor/kuma-canvas.js` is shared by every project that draws, so what its helpers
// return is part of every finished video: a value that changes re-draws overlays nobody
// edited. These pin the values, and check that the mixed helpers do what the first ones
// did not. Pure JavaScript: no browser, no daemon.

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";

import { REPO } from "./harness.mjs";

/** Canvases that record nothing but can be told apart. */
const document = { createElement: () => ({ getContext: () => ({}) }) };
const window = {};
vm.runInNewContext(fs.readFileSync(path.join(REPO, "library/blueprints/_vendor/kuma-canvas.js"), "utf8"), { window, document, Math });
const K = window.kumaCanvas;

/** A drawn overlay on a canvas that draws nothing: `at(t)` runs one frame and returns its `g`. */
function overlay() {
  let draw;
  const tpl = { width: 64, height: 36, vars: {}, onFrame: (fn) => (draw = fn) };
  const ctx = { save() {}, restore() {}, clearRect() {} };
  let g;
  K(tpl, { getContext: () => ctx }, (got) => (g = got));
  return { at: (t) => (draw(t), g) };
}
const frame = (t) => overlay().at(t);

/** 400 stars placed at (f(`sx${i}`), f(`sy${i}`)): how many cells of a 10×10 grid hold one. */
const cellsFilled = (f) => new Set(Array.from({ length: 400 }, (_, i) => Math.floor(f(`sx${i}`) * 10) * 10 + Math.floor(f(`sy${i}`) * 10))).size;

describe("kuma-canvas random values", () => {
  it("keeps the values the first helpers returned", () => {
    assert.equal(K.hash("delay0"), 0.9863252770155668);
    assert.equal(K.hash("star1"), 0.6256619542837143);
    assert.equal(K.hash(7), 0.19550537830218673);
    assert.equal(frame(0.5).rand("a"), frame(0.5).seed("a") / 4294967296);
  });

  it("keeps the values of the mixed ones too", () => {
    assert.equal(K.rand("delay0"), 0.12213588343001902);
    assert.equal(K.rand("star1"), 0.8393113499041647);
    assert.equal(K.rand(7), 0.47707845945842564);
  });

  it("gives alike keys unrelated values, where hash lays a star field in lines", () => {
    // Measured: hash fills 32 of the 100 cells, rand all of them.
    assert.ok(cellsFilled(K.hash) < 50, "hash no longer shows the defect this test is about");
    assert.ok(cellsFilled(K.rand) > 90, `rand filled ${cellsFilled(K.rand)} cells`);
    const g = frame(0.5);
    assert.ok(cellsFilled(g.jitter) > 90, `jitter filled ${cellsFilled(g.jitter)} cells`);
  });

  it("holds jitter for one drawing and changes it with the next", () => {
    // 12 drawings a second: 0.50 and 0.52 are one drawing, 0.60 the next.
    assert.equal(frame(0.5).jitter("a"), frame(0.52).jitter("a"));
    assert.notEqual(frame(0.5).jitter("a"), frame(0.6).jitter("a"));
  });
});

describe("kuma-canvas loops, stills and colours", () => {
  it("fits whole cycles to any length, and each loop ends where it began", () => {
    assert.equal(K.cycles(10, 3), 3);
    assert.equal(K.cycles(1, 3), 1, "never zero cycles");
    assert.equal(K.phase(2.5, 10, 2), 0.5);
    assert.equal(K.phase(10, 10, 3), 0);
    for (const n of [1, 3]) {
      assert.equal(K.loopNoise(0.3, 0.7, 10, 10, n), K.loopNoise(0.3, 0.7, 0, 10, n), `n = ${n}`);
      assert.notEqual(K.loopNoise(0.3, 0.7, 2.5 / n, 10, n), K.loopNoise(0.3, 0.7, 0, 10, n), "it moves in between");
    }
    assert.equal(K.loopNoise(0.3, 0.7, 2.5, 10, 2), 0.11153946515746414);
  });

  it("paints a still once, the same whichever frame asks first", () => {
    const first = overlay(), later = overlay();
    let painted = 0;
    const paint = (into) => () => { painted++; into.push(Math.random(), Math.random()); };
    const fromStart = [], fromMiddle = [];
    const a = first.at(0).still("sky", paint(fromStart));
    assert.equal(first.at(1.5).still("sky", paint(fromStart)), a, "kept, not painted again");
    const b = later.at(2.75).still("sky", paint(fromMiddle));
    assert.equal(painted, 2, "once per overlay");
    assert.deepEqual(fromMiddle, fromStart, "seeded by its key, not by the frame that asked");
    assert.notEqual(later.at(2.75).still("sea", () => {}), b, "a key is a still");
  });

  it("writes colours as CSS", () => {
    assert.equal(K.rgba("#ff8000", 0.5), "rgba(255, 128, 0, 0.5)");
    assert.equal(K.rgba("#f80c", 0.5), "rgba(255, 136, 0, 0.4)", "its own alpha times the one given");
    assert.equal(K.mix("#000000", "#ffffff", 0.5), "rgba(128, 128, 128, 1)");
    assert.equal(K.mix("#ff000000", "#ff0000", 0.25), "rgba(255, 0, 0, 0.25)");
    assert.equal(K.mix("#000", "#fff", 2), "rgba(255, 255, 255, 1)", "k is clamped");
  });
});
