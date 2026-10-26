// The scopes panel's counting: where a frame's pixels land on the waveform, the parade, the
// vectorscope and the histogram. No browser needed.

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";

import { analyse, vectorAt } from "../src/scopes.ts";

/** A `w × h` frame of one colour, left half and right half. */
const frame = (w, h, left, right) => {
  const px = new Uint8Array(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) px.set([...(x < w / 2 ? left : right), 255], (y * w + x) * 4);
  return px;
};

describe("scopes", () => {
  it("put each half of the frame in its own columns, at its luma and its channels", () => {
    const s = analyse(frame(8, 2, [255, 255, 255], [0, 0, 0]), 8, 2, 4, 32);
    // White on the left two columns at 255, black on the right two at 0: two rows of four pixels each.
    assert.equal(s.waveform[0 * 256 + 255], 4);
    assert.equal(s.waveform[3 * 256 + 0], 4);
    const red = (col, level) => s.parade[col * 256 + level];
    assert.equal(red(1, 255) + red(2, 0), 8);
    assert.equal(s.histogram[768 + 255], 8);
    assert.equal(s.histogram[768 + 0], 8);
  });

  it("put greys at the vectorscope's centre and colours towards their hue", () => {
    const [cx, cy] = vectorAt(128, 128, 128, 65);
    assert.deepEqual([cx, cy], [32, 32]);
    const [rx, ry] = vectorAt(255, 0, 0, 65);
    assert.ok(rx < 32 && ry < 32, "red is up and to the left");
    const [bx] = vectorAt(0, 0, 255, 65);
    assert.ok(bx > 32, "blue is to the right");
    const s = analyse(frame(4, 1, [128, 128, 128], [128, 128, 128]), 4, 1, 4, 65);
    assert.equal(s.vector[32 * 65 + 32], 4);
  });
});
