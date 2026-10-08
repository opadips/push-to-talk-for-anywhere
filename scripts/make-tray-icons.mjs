// Generates assets/tray-{talking,muted,disabled}.png (32x32 RGBA) — the
// three distinct tray states of plan §9 M4.
//
// Everything is drawn in a 512-unit design space and sampled 4x4 per output
// pixel, so the glyph stays smooth at the 16 px the taskbar usually asks
// for. Same dark tile and microphone mark as assets/app-icon.png, so the
// tray matches the app icon; the state shows as a live dot (talking), a
// slash (muted) or a dimmed microphone (disabled).
//
// Run: node scripts/make-tray-icons.mjs
import { writeFileSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { encodePng } from "./lib/png.mjs";

const OUT = 32; // what the shell scales from (16 px at 100 %, 32 at 200 %)
const SS = 4; // samples per pixel edge — anti-aliasing
const VW = 512; // design space the shapes below are written in

const TILE = [31, 35, 40, 255]; // #1f2328 — the app icon's dark tile
const MIC = [232, 234, 237, 255]; // #e8eaed — the app icon's microphone
const MIC_OFF = [110, 118, 129, 255]; // #6e7681 — dimmed, "not listening"
const LIVE = [129, 201, 149, 255]; // #81c995 — talking
const MUTE = [242, 139, 130, 255]; // #f28b82 — muted

// --- shapes (design space) ------------------------------------------------
function roundRect(cx, cy, hw, hh, r) {
  return (x, y) => {
    const dx = Math.abs(x - cx) - (hw - r);
    const dy = Math.abs(y - cy) - (hh - r);
    return Math.hypot(Math.max(dx, 0), Math.max(dy, 0)) + Math.min(Math.max(dx, dy), 0) - r <= 0;
  };
}

function circle(cx, cy, r) {
  return (x, y) => Math.hypot(x - cx, y - cy) <= r;
}

/// Everything within `thickness` of the segment — the mute slash.
function segment(x1, y1, x2, y2, thickness) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const len2 = dx * dx + dy * dy;
  return (x, y) => {
    const t = Math.min(1, Math.max(0, ((x - x1) * dx + (y - y1) * dy) / len2));
    return Math.hypot(x - (x1 + t * dx), y - (y1 + t * dy)) <= thickness;
  };
}

/// Lower half of a ring: the bracket the microphone sits in.
function bracket(cx, cy, outer, inner) {
  return (x, y) => {
    if (y < cy) return false;
    const d = Math.hypot(x - cx, y - cy);
    return d <= outer && d >= inner;
  };
}

// The shared microphone: capsule in a bracket on a stand (as in the app
// icon), bold enough to survive the shrink to 16 px.
const capsule = roundRect(256, 172, 66, 104, 66);
const standTop = roundRect(256, 384, 84, 22, 22);
const standStem = roundRect(256, 434, 20, 40, 20);

function microphone(color) {
  return [
    { shape: capsule, color },
    { shape: bracket(256, 216, 156, 118), color },
    { shape: standTop, color },
    { shape: standStem, color },
  ];
}

const STATES = {
  // Live: the microphone plus the green dot the app icon uses.
  talking: [...microphone(MIC), { shape: circle(404, 108, 44), color: LIVE }],
  // Muted: the microphone crossed out.
  muted: [...microphone(MIC), { shape: segment(150, 380, 362, 168, 34), color: MUTE }],
  // Disabled: the same mark, dimmed, with nothing lit.
  disabled: microphone(MIC_OFF),
};

// --- render ---------------------------------------------------------------
function render(layers) {
  const px = new Uint8Array(OUT * OUT * 4);
  const tile = roundRect(256, 256, 256, 256, 112); // drawn under everything
  const all = [{ shape: tile, color: TILE }, ...layers];

  for (let oy = 0; oy < OUT; oy++) {
    for (let ox = 0; ox < OUT; ox++) {
      let r = 0;
      let g = 0;
      let b = 0;
      let covered = 0;

      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const x = ((ox + (sx + 0.5) / SS) / OUT) * VW;
          const y = ((oy + (sy + 0.5) / SS) / OUT) * VW;
          // The topmost shape that covers the sample wins.
          let color = null;
          for (const layer of all) if (layer.shape(x, y)) color = layer.color;
          if (color) {
            r += color[0];
            g += color[1];
            b += color[2];
            covered++;
          }
        }
      }

      const i = (oy * OUT + ox) * 4;
      if (covered === 0) continue; // fully transparent corner
      // Colour is the average of the covered samples, alpha their share:
      // that composites without a dark fringe.
      px[i] = Math.round(r / covered);
      px[i + 1] = Math.round(g / covered);
      px[i + 2] = Math.round(b / covered);
      px[i + 3] = Math.round((255 * covered) / (SS * SS));
    }
  }
  return px;
}

for (const [state, layers] of Object.entries(STATES)) {
  const out = resolve(`assets/tray-${state}.png`);
  const png = encodePng(OUT, OUT, render(layers));
  mkdirSync(resolve("assets"), { recursive: true });
  writeFileSync(out, png);
  console.log(`wrote ${out} (${png.length} bytes)`);
}
