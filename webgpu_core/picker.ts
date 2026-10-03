/**
 * picker.ts — screen-space picking for point-like objects.
 *
 * Stars are points, so "what's under the mouse" is: project every candidate to the screen and
 * take the closest one within its on-screen radius (plus a little slack). Tens of thousands of
 * projections per pointer move is well under a millisecond.
 */

import { transformPoint } from './math.js';

export interface ScreenPoint { x: number; y: number; depth: number; visible: boolean }

const tmp = new Float32Array(4);

/** Project a world point to CSS pixels inside a viewport of `cssW` x `cssH`. */
export function projectToScreen(viewProj: ArrayLike<number>, x: number, y: number, z: number, cssW: number, cssH: number, out?: ScreenPoint): ScreenPoint {
  const p = out ?? { x: 0, y: 0, depth: 0, visible: false };
  transformPoint(viewProj, x, y, z, tmp);
  const w = tmp[3];
  if (w <= 0.0001) { p.visible = false; return p; }
  const nx = tmp[0] / w, ny = tmp[1] / w, nz = tmp[2] / w;
  p.x = (nx * 0.5 + 0.5) * cssW;
  p.y = (1 - (ny * 0.5 + 0.5)) * cssH;
  p.depth = w;
  p.visible = nz >= 0 && nz <= 1 && nx > -1.2 && nx < 1.2 && ny > -1.2 && ny < 1.2;
  return p;
}

/**
 * Find the instance nearest to (mouseX, mouseY) in CSS pixels.
 * `data` uses the star instance layout (stride floats, position at 0..2, radius at 3).
 * `projScaleCss` = P[1][1] * cssHeight / 2, so `radius * projScaleCss / w` is the radius in CSS px.
 * Returns -1 when nothing is within reach.
 */
export function pickNearest(
  viewProj: ArrayLike<number>, data: Float32Array, stride: number, count: number,
  mouseX: number, mouseY: number, cssW: number, cssH: number, projScaleCss: number,
  minRadiusPx = 6, filter?: (index: number) => boolean,
): number {
  let best = -1;
  let bestScore = Infinity;
  for (let i = 0; i < count; i++) {
    if (filter && !filter(i)) continue;
    const o = i * stride;
    transformPoint(viewProj, data[o], data[o + 1], data[o + 2], tmp);
    const w = tmp[3];
    if (w <= 0.0001) continue;
    const sx = (tmp[0] / w * 0.5 + 0.5) * cssW;
    const sy = (1 - (tmp[1] / w * 0.5 + 0.5)) * cssH;
    const dx = sx - mouseX, dy = sy - mouseY;
    const d2 = dx * dx + dy * dy;
    const coreR = Math.max(minRadiusPx, data[o + 3] * projScaleCss / w * 0.45);
    if (d2 > coreR * coreR) continue;
    // Prefer the closest on screen; break ties toward the nearer star.
    const score = d2 + w * 1e-4;
    if (score < bestScore) { bestScore = score; best = i; }
  }
  return best;
}
