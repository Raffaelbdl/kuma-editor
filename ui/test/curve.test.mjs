// The curve field's maths against the compiler's: `ui/src/curve.ts` is a copy of `Curve` in
// `crates/kuma-core/src/grade.rs`, and both are pinned to these values (the Rust side in
// `the_curve_draws_what_the_editor_draws`). No browser needed: Node runs the module as is.

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";

import { curveThrough, withCorners, withoutCorners } from "../src/curve.ts";

const XS = [0.0, 0.1, 0.2, 0.3, 0.35, 0.5, 0.52, 0.6, 0.7, 0.8, 0.9, 1.0];
const CASES = [
  [[[0.0, 0.05], [0.2, 0.1], [0.5, 0.5], [0.55, 0.9], [0.8, 0.92]],
    [0.05, 0.065497641, 0.1, 0.142771436, 0.174453941, 0.5, 0.668117659, 0.909632625, 0.918197499, 0.92, 0.950355808, 1.0]],
  [[[0.25, 0.22], [0.75, 0.8]], [0.0, 0.08464, 0.17152, 0.27314, 0.32992, 0.5125, 0.53727376, 0.63482, 0.74866, 0.84576, 0.92432, 1.0]],
];

describe("the curve field's curve", () => {
  it("is the compiler's, value for value", () => {
    for (const [points, want] of CASES) {
      const c = curveThrough(points);
      XS.forEach((x, i) => assert.ok(Math.abs(c(x) - want[i]) < 1e-8, `${JSON.stringify(points)} at ${x}: ${c(x)} not ${want[i]}`));
    }
  });

  it("is no curve when it is the straight line", () => {
    assert.equal(curveThrough([]), null);
    assert.equal(curveThrough([[0.5, 0.5]]), null);
  });

  it("shows the corners it passes through, and stores only the points that were set", () => {
    assert.deepEqual(withCorners([[0.5, 0.6]]), [[0, 0], [0.5, 0.6], [1, 1]]);
    assert.deepEqual(withCorners([[0, 0.1], [1, 0.9]]), [[0, 0.1], [1, 0.9]]);
    assert.deepEqual(withoutCorners([[0, 0], [0.5, 0.6], [1, 1]]), [[0.5, 0.6]]);
    assert.deepEqual(withoutCorners([[0, 0.1], [0.5, 0.6], [1, 1]]), [[0, 0.1], [0.5, 0.6]]);
  });
});
