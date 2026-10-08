// Generates assets/app-icon.png (512x512 RGBA) — a simple microphone mark.
// Run: node scripts/make-app-icon.mjs [output-path]
import { deflateSync, crc32 } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";

const SIZE = 512;
const out = resolve(process.argv[2] ?? "assets/app-icon.png");

// --- drawing -------------------------------------------------------------
const px = new Uint8Array(SIZE * SIZE * 4); // RGBA, premultiplied-free

function put(x, y, [r, g, b, a = 255]) {
  if (x < 0 || y < 0 || x >= SIZE || y >= SIZE) return;
  const i = (y * SIZE + x) * 4;
  // alpha-blend src over existing dst
  const sa = a / 255;
  const da = px[i + 3] / 255;
  const oa = sa + da * (1 - sa);
  if (oa === 0) return;
  px[i] = Math.round((r * sa + px[i] * da * (1 - sa)) / oa);
  px[i + 1] = Math.round((g * sa + px[i + 1] * da * (1 - sa)) / oa);
  px[i + 2] = Math.round((b * sa + px[i + 2] * da * (1 - sa)) / oa);
  px[i + 3] = Math.round(oa * 255);
}

function fill(shape, color) {
  for (let y = 0; y < SIZE; y++)
    for (let x = 0; x < SIZE; x++) if (shape(x, y)) put(x, y, color);
}

// signed distance to an axis-aligned rounded rectangle
function roundRect(cx, cy, hw, hh, r) {
  return (x, y) => {
    const dx = Math.abs(x - cx) - (hw - r);
    const dy = Math.abs(y - cy) - (hh - r);
    const ax = Math.max(dx, 0);
    const ay = Math.max(dy, 0);
    return Math.hypot(ax, ay) + Math.min(Math.max(dx, dy), 0) - r <= 0;
  };
}

function circle(cx, cy, r) {
  return (x, y) => Math.hypot(x - cx, y - cy) <= r;
}

const BG = [31, 35, 40, 255]; // #1f2328
const FG = [232, 234, 237, 255]; // #e8eaed
const GREEN = [129, 201, 149, 255]; // #81c995 (talking)

// rounded-square background
fill(roundRect(256, 256, 256, 256, 112), BG);

// microphone capsule
fill(roundRect(256, 200, 66, 108, 64), FG);
// stand: bar + stem
fill(roundRect(256, 340, 96, 16, 16), FG);
fill(roundRect(256, 384, 16, 44, 16), FG);
// arc hint (a simple ring segment drawn as a circle outline)
const ring = (x, y) => {
  const d = Math.hypot(x - 256, y - 236);
  return d <= 150 && d >= 128;
};
fill(ring, FG);
// "live" dot
fill(circle(392, 128, 34), GREEN);

// --- PNG encoding --------------------------------------------------------
const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
for (let y = 0; y < SIZE; y++) {
  const rowStart = y * (SIZE * 4 + 1);
  raw[rowStart] = 0; // filter: none
  Buffer.from(px.buffer, y * SIZE * 4, SIZE * 4).copy(raw, rowStart + 1);
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "latin1"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body) >>> 0);
  return Buffer.concat([len, body, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // RGBA
ihdr[10] = 0;
ihdr[11] = 0;
ihdr[12] = 0;

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, png);
console.log(`wrote ${out} (${png.length} bytes)`);
