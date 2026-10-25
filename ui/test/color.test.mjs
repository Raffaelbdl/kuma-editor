// The colour fields' maths: hex with alpha (which a native colour input drops), the
// picker's hue/saturation/value, and the grade wheel's plane. No browser needed.

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";

import { hsvToRgb, offsetsToWheel, parseHex, rgbToHsv, toHex, wheelToOffsets } from "../src/color.ts";

const close = (a, b, eps = 1e-9) => Math.abs(a - b) < eps;

describe("colour fields", () => {
  it("read every hex form the model takes, and keep alpha", () => {
    assert.deepEqual(parseHex("#ff4f00"), { r: 255, g: 79, b: 0, a: 255 });
    assert.deepEqual(parseHex("#ff4f00cc"), { r: 255, g: 79, b: 0, a: 204 });
    assert.deepEqual(parseHex("#abc"), { r: 170, g: 187, b: 204, a: 255 });
    assert.deepEqual(parseHex("abcd"), { r: 170, g: 187, b: 204, a: 221 });
    for (const bad of ["", "#ff4f0", "#gg0000", "red"]) assert.equal(parseHex(bad), null, bad);
    assert.equal(toHex({ r: 255, g: 79, b: 0, a: 255 }), "#ff4f00");
    assert.equal(toHex({ r: 255, g: 79, b: 0, a: 204 }), "#ff4f00cc");
  });

  it("go round hue, saturation and value without drifting", () => {
    for (const hex of ["#ff4f00", "#3b82f6", "#808080", "#000000", "#ffffff", "#a08060"]) {
      const c = parseHex(hex);
      assert.equal(toHex(hsvToRgb(rgbToHsv(c))), hex);
    }
    assert.deepEqual(rgbToHsv(parseHex("#ff0000")), { h: 0, s: 1, v: 1 });
    assert.ok(close(rgbToHsv(parseHex("#00ff00")).h, 120) && close(rgbToHsv(parseHex("#0000ff")).h, 240));
  });

  it("put a wheel's colour in the plane and its brightness in the master, and back", () => {
    const red = offsetsToWheel({ r: 0.1, g: 0, b: 0 });
    assert.ok(red.x > 0 && close(red.y, 0) && close(red.master, 0.1 / 3), JSON.stringify(red));
    const green = offsetsToWheel({ r: 0, g: 0.1, b: 0 });
    assert.ok(green.x < 0 && green.y > 0);
    const grey = offsetsToWheel({ r: 0.05, g: 0.05, b: 0.05 });
    assert.ok(close(grey.x, 0) && close(grey.y, 0) && close(grey.master, 0.05));
    for (const o of [{ r: 0.1, g: -0.02, b: 0.03 }, { r: -0.2, g: 0.15, b: 0.01 }]) {
      const w = offsetsToWheel(o);
      const back = wheelToOffsets(w.x, w.y, w.master);
      assert.ok(close(back.r, o.r) && close(back.g, o.g) && close(back.b, o.b), JSON.stringify([o, back]));
    }
  });
});
