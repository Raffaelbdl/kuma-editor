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

const window = {};
vm.runInNewContext(fs.readFileSync(path.join(REPO, "library/blueprints/_vendor/kuma-canvas.js"), "utf8"), { window, Math });
const K = window.kumaCanvas;

/** `g` for one frame at `t`, from a canvas that draws nothing. */
function frame(t) {
  let draw;
  const tpl = { width: 64, height: 36, vars: {}, onFrame: (fn) => (draw = fn) };
  const ctx = { save() {}, restore() {}, clearRect() {} };
  let g;
  K(tpl, { getContext: () => ctx }, (got) => (g = got));
  draw(t);
  return g;
}

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
