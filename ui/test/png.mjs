// A minimal PNG reader, so the composite test can look at pixels without an image library
// or ImageMagick. Handles what Chromium's `--screenshot` emits: 8-bit RGB or RGBA.

import zlib from "node:zlib";

function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

/** Decode to `{ width, height, at(x, y) -> [r, g, b] }`. */
export function decodePng(buf) {
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error("not a PNG");
  let pos = 8, width = 0, height = 0, channels = 0;
  const idat = [];
  while (pos < buf.length) {
    const len = buf.readUInt32BE(pos);
    const type = buf.toString("ascii", pos + 4, pos + 8);
    const data = buf.subarray(pos + 8, pos + 8 + len);
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      const depth = data[8], colour = data[9];
      if (depth !== 8) throw new Error(`unsupported bit depth ${depth}`);
      if (colour === 2) channels = 3;
      else if (colour === 6) channels = 4;
      else throw new Error(`unsupported colour type ${colour}`);
      if (data[12] !== 0) throw new Error("interlaced PNGs are not supported");
    } else if (type === "IDAT") idat.push(data);
    else if (type === "IEND") break;
    pos += 12 + len;
  }

  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const out = Buffer.alloc(height * stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let i = 0; i < stride; i++) {
      const a = i >= channels ? out[y * stride + i - channels] : 0;
      const b = y > 0 ? out[(y - 1) * stride + i] : 0;
      const c = i >= channels && y > 0 ? out[(y - 1) * stride + i - channels] : 0;
      const x = line[i];
      out[y * stride + i] =
        filter === 0 ? x
        : filter === 1 ? x + a
        : filter === 2 ? x + b
        : filter === 3 ? x + ((a + b) >> 1)
        : filter === 4 ? x + paeth(a, b, c)
        : (() => { throw new Error(`unknown filter ${filter}`); })();
    }
  }
  return {
    width,
    height,
    at(x, y) {
      const i = y * stride + x * channels;
      return [out[i], out[i + 1], out[i + 2]];
    },
  };
}

/** How many pixels satisfy `pred`, over the whole image. */
export function count(img, pred) {
  let n = 0;
  for (let y = 0; y < img.height; y++) for (let x = 0; x < img.width; x++) if (pred(img.at(x, y))) n++;
  return n;
}
