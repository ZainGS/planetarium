/**
 * scene.ts — turn repo layouts into the flat arrays the GPU passes draw, and keep the mapping
 * from star index back to (repo, tree node) for picking, labels and the details panel.
 */

import { STAR_FLOATS, LINE_VERTEX_FLOATS } from '../../webgpu_core/index.js';
import type { RepoLayout, LayoutNode } from './layout.js';
import { repoColor, tint, type RGB } from './palette.js';

export interface SceneRepo {
  id: string;
  name: string;
  colorIndex: number;
  layout: RepoLayout;
  center: [number, number, number];
}

export interface BuiltScene {
  stars: Float32Array;
  starCount: number;
  lines: Float32Array;
  lineVertexCount: number;
  /** For each star: index into `repos`, and index into that repo's layout nodes. */
  starRepo: Int32Array;
  starNode: Int32Array;
  /** First star index of each repo (its root star). */
  repoFirstStar: number[];
  repos: SceneRepo[];
  /** Radius of a sphere around the origin enclosing every constellation. */
  extent: number;
  /** Per line vertex: the star it's attached to (-1 for orbit-ring vertices). */
  lineStar: Int32Array;
  /** Per line vertex: the child star of its edge, whose fade-in the edge follows (-1 for rings). */
  lineChild: Int32Array;
  /** Per line vertex: index into `repos`. */
  lineRepo: Int32Array;
}

/** Stable identity of a star across rebuilds: repo id + path inside the repo. */
export function starKey(scene: BuiltScene, star: number): string {
  const repo = scene.repos[scene.starRepo[star]];
  return `${repo.id}\u0000${repo.layout.nodes[scene.starNode[star]].node.path}`;
}

const RING_SEGMENTS = 160;

function writeStar(buf: Float32Array, i: number, x: number, y: number, z: number, size: number, c: RGB, alpha: number, minPx: number, core: number): void {
  const o = i * STAR_FLOATS;
  buf[o] = x; buf[o + 1] = y; buf[o + 2] = z; buf[o + 3] = size;
  buf[o + 4] = c[0]; buf[o + 5] = c[1]; buf[o + 6] = c[2]; buf[o + 7] = alpha;
  buf[o + 8] = 0; buf[o + 9] = minPx; buf[o + 10] = core; buf[o + 11] = 0;
}

function writeLineVertex(buf: Float32Array, i: number, x: number, y: number, z: number, c: RGB, a: number): void {
  const o = i * LINE_VERTEX_FLOATS;
  buf[o] = x; buf[o + 1] = y; buf[o + 2] = z;
  buf[o + 3] = c[0]; buf[o + 4] = c[1]; buf[o + 5] = c[2]; buf[o + 6] = a;
}

export function buildScene(repos: SceneRepo[]): BuiltScene {
  let starCount = 0;
  let lineVerts = 0;
  for (const r of repos) {
    starCount += r.layout.nodes.length;
    lineVerts += (r.layout.nodes.length - 1) * 2 + RING_SEGMENTS * 2;
  }

  const stars = new Float32Array(Math.max(starCount, 1) * STAR_FLOATS);
  const lines = new Float32Array(Math.max(lineVerts, 1) * LINE_VERTEX_FLOATS);
  const starRepo = new Int32Array(starCount);
  const starNode = new Int32Array(starCount);
  const repoFirstStar: number[] = [];
  const lineStar = new Int32Array(lineVerts);
  const lineChild = new Int32Array(lineVerts);
  const lineRepo = new Int32Array(lineVerts);

  let s = 0;
  let l = 0;
  let extent = 0;

  repos.forEach((repo, ri) => {
    const [cx, cy, cz] = repo.center;
    const base = repoColor(repo.colorIndex);
    const coreColor = tint(base, 0.55);
    const folderColor = tint(base, 0.12);
    const fileColor = tint(base, -0.25);
    const nodes = repo.layout.nodes;
    const first = s;
    repoFirstStar.push(s);
    extent = Math.max(extent, Math.hypot(cx, cy, cz) + repo.layout.radius);

    nodes.forEach((n: LayoutNode, ni) => {
      const x = cx + n.x, y = cy + n.y, z = cz + n.z;
      if (ni === 0) writeStar(stars, s, x, y, z, n.size, coreColor, 1.0, 7, 0.22);
      else if (n.node.isDir) writeStar(stars, s, x, y, z, n.size, folderColor, 1.0, 3.2, 0.26);
      else writeStar(stars, s, x, y, z, n.size, fileColor, 0.7, 1.8, 0.34);
      starRepo[s] = ri;
      starNode[s] = ni;
      s++;

      if (n.parent >= 0) {
        const p = nodes[n.parent];
        const isFolder = n.node.isDir;
        const aStart = isFolder ? (p.parent < 0 ? 0.34 : 0.26) : 0.075;
        const aEnd = isFolder ? 0.14 : 0.02;
        const child = first + ni;
        lineStar[l] = first + n.parent; lineChild[l] = child; lineRepo[l] = ri;
        writeLineVertex(lines, l++, cx + p.x, cy + p.y, cz + p.z, base, aStart);
        lineStar[l] = child; lineChild[l] = child; lineRepo[l] = ri;
        writeLineVertex(lines, l++, x, y, z, base, aEnd);
      }
    });

    // A faint orbit ring around the constellation. Idle agents will park on it later.
    const rr = repo.layout.radius * 1.04;
    for (let k = 0; k < RING_SEGMENTS; k++) {
      const a0 = (k / RING_SEGMENTS) * Math.PI * 2;
      const a1 = ((k + 1) / RING_SEGMENTS) * Math.PI * 2;
      // Brighter on one side so the ring reads as an orbit rather than a flat circle.
      const fade = (a: number) => 0.05 + 0.07 * (0.5 + 0.5 * Math.cos(a - 0.8));
      lineStar[l] = -1; lineChild[l] = -1; lineRepo[l] = ri;
      writeLineVertex(lines, l++, cx + Math.cos(a0) * rr, cy, cz + Math.sin(a0) * rr, base, fade(a0));
      lineStar[l] = -1; lineChild[l] = -1; lineRepo[l] = ri;
      writeLineVertex(lines, l++, cx + Math.cos(a1) * rr, cy, cz + Math.sin(a1) * rr, base, fade(a1));
    }
  });

  return { stars, starCount, lines, lineVertexCount: l, starRepo, starNode, repoFirstStar, repos, extent: Math.max(extent, 10), lineStar, lineChild, lineRepo };
}

/** A few thousand faint background stars on a unit sphere (drawn with a rotation-only camera). */
export function buildSkyStars(count = 2200, seed = 1337): Float32Array {
  const out = new Float32Array(count * STAR_FLOATS);
  let st = seed >>> 0;
  const rand = () => {
    st = (st + 0x6d2b79f5) >>> 0;
    let t = st;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  for (let i = 0; i < count; i++) {
    const z = rand() * 2 - 1;
    const phi = rand() * Math.PI * 2;
    const r = Math.sqrt(1 - z * z);
    const bright = Math.pow(rand(), 3.2);
    const warm = rand();
    const c: RGB = warm < 0.2 ? [1.0, 0.86, 0.72] : warm > 0.8 ? [0.75, 0.84, 1.0] : [0.92, 0.94, 1.0];
    writeStar(out, i, r * Math.cos(phi) * 50, z * 50, r * Math.sin(phi) * 50, 0.001, c, 0.12 + bright * 0.55, 0.9 + bright * 1.4, 0.45);
  }
  return out;
}
