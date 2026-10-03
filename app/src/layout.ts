/**
 * layout.ts — deterministic 3D positions for a repo's folder tree.
 *
 * The repo root is a bright core star. Top-level folders spread around it on a sphere; deeper
 * folders branch outward in a cone that points away from their parent, so each subtree grows
 * like a limb of the constellation. Files sit in a small shell around their folder, at an angle
 * derived from a hash of the file path, so adding or removing a file almost never moves its siblings.
 *
 * Everything is a pure function of the file list: the same repo always looks the same.
 */

import type { TreeNode } from './tree.js';

export interface LayoutNode {
  node: TreeNode;
  /** Index of the parent in `RepoLayout.nodes`, -1 for the root. */
  parent: number;
  x: number;
  y: number;
  z: number;
  /** Glow radius in world units. */
  size: number;
}

export interface RepoLayout {
  nodes: LayoutNode[];
  /** Distance from the constellation's center to the farthest star, plus a margin. */
  radius: number;
  fileCount: number;
  folderCount: number;
}

const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5));
/** Flatten the vertical axis a little so constellations read as discs from an oblique camera. */
const Y_SQUASH = 0.6;

/** FNV-1a, 32-bit. */
export function hashString(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Deterministic value in [0, 1) from a string and a salt. */
export function hash01(s: string, salt = 0): number {
  let h = hashString(s) ^ Math.imul(salt + 1, 0x9e3779b1);
  h = Math.imul(h ^ (h >>> 16), 0x85ebca6b);
  h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
  h ^= h >>> 16;
  return (h >>> 0) / 4294967296;
}

type V3 = [number, number, number];

function normalize(v: V3): V3 {
  const l = Math.hypot(v[0], v[1], v[2]) || 1;
  return [v[0] / l, v[1] / l, v[2] / l];
}

/** Two unit vectors perpendicular to `d` (and to each other). */
function basis(d: V3): [V3, V3] {
  const a: V3 = Math.abs(d[1]) < 0.9 ? [0, 1, 0] : [1, 0, 0];
  const u = normalize([d[1] * a[2] - d[2] * a[1], d[2] * a[0] - d[0] * a[2], d[0] * a[1] - d[1] * a[0]]);
  const v: V3 = [d[1] * u[2] - d[2] * u[1], d[2] * u[0] - d[0] * u[2], d[0] * u[1] - d[1] * u[0]];
  return [u, v];
}

/** Direction at polar angle `theta` from `d`, rotated `phi` around it. */
function coneDir(d: V3, theta: number, phi: number): V3 {
  const [u, v] = basis(d);
  const s = Math.sin(theta), c = Math.cos(theta);
  const cp = Math.cos(phi), sp = Math.sin(phi);
  return normalize([
    d[0] * c + (u[0] * cp + v[0] * sp) * s,
    d[1] * c + (u[1] * cp + v[1] * sp) * s,
    d[2] * c + (u[2] * cp + v[2] * sp) * s,
  ]);
}

/** Uniformly distributed direction from two hash values. */
function hashedDir(key: string): V3 {
  const z = hash01(key, 1) * 2 - 1;
  const phi = hash01(key, 2) * Math.PI * 2;
  const r = Math.sqrt(1 - z * z);
  return [r * Math.cos(phi), z, r * Math.sin(phi)];
}

export function folderStarSize(fileCount: number): number {
  return 0.6 + 0.2 * Math.log2(1 + fileCount);
}

export const FILE_STAR_SIZE = 0.3;
export const ROOT_STAR_SIZE = 2.4;

export function layoutRepo(root: TreeNode): RepoLayout {
  const nodes: LayoutNode[] = [];
  nodes.push({ node: root, parent: -1, x: 0, y: 0, z: 0, size: ROOT_STAR_SIZE });

  const place = (index: number, dir: V3 | null): void => {
    const self = nodes[index];
    const node = self.node;
    const depth = node.depth;
    const folders = node.children.filter((c) => c.isDir);
    const files = node.children.filter((c) => !c.isDir);

    // ── Sub-folders: branch outward ─────────────────────────────
    // Each folder claims a slot (by name hash) out of a fixed-size set that only grows in
    // powers of two, so adding or removing a folder leaves its siblings where they are.
    const n = folders.length;
    const twist = hash01(node.path, 7) * Math.PI * 2;
    const slots = n <= 1 ? 1 : Math.max(4, 1 << Math.ceil(Math.log2(n * 2)));
    const taken = new Uint8Array(slots);
    const spread = dir ? Math.min(1.3, 0.42 + 0.13 * Math.sqrt(slots / 2)) : Math.PI;
    for (let i = 0; i < n; i++) {
      const child = folders[i];
      let j = hashString(child.name) % slots;
      while (taken[j]) j = (j + 1) % slots;
      taken[j] = 1;
      let d: V3;
      if (!dir) {
        // Root: slots spread evenly over the sphere (Fibonacci lattice).
        if (n === 1) d = hashedDir(child.path);
        else {
          const y = 1 - (2 * (j + 0.5)) / slots;
          const r = Math.sqrt(Math.max(0, 1 - y * y));
          const phi = j * GOLDEN_ANGLE + twist;
          d = [r * Math.cos(phi), y, r * Math.sin(phi)];
        }
      } else {
        const t = (j + 0.5) / slots;
        const theta = n === 1 ? 0.18 + 0.25 * hash01(child.path, 3) : spread * Math.sqrt(t);
        d = coneDir(dir, theta, j * GOLDEN_ANGLE + twist);
      }
      const base = depth === 0 ? 8.5 : 6.2 * Math.pow(0.8, depth - 1);
      const len = base * (0.7 + 0.32 * Math.log10(1 + child.fileCount)) * (0.92 + 0.16 * hash01(child.path, 4));
      nodes.push({
        node: child,
        parent: index,
        x: self.x + d[0] * len,
        y: self.y + d[1] * len,
        z: self.z + d[2] * len,
        size: folderStarSize(child.fileCount),
      });
      place(nodes.length - 1, d);
    }

    // ── Files: a small hashed shell around the folder ───────────
    const m = files.length;
    if (m > 0) {
      // Shell size grows in steps (next power of two), so a new file rarely nudges its siblings.
      const bucket = Math.pow(2, Math.ceil(Math.log2(Math.max(1, m))));
      const shell = (depth === 0 ? 1.6 : 0.7) + 0.13 * Math.sqrt(bucket);
      for (const f of files) {
        let d = hashedDir(f.path);
        // Lean files away from the parent so they don't sit on the incoming branch.
        if (dir) d = normalize([d[0] + dir[0] * 0.6, d[1] + dir[1] * 0.6, d[2] + dir[2] * 0.6]);
        const r = shell * (0.55 + 0.45 * hash01(f.path, 5));
        nodes.push({ node: f, parent: index, x: self.x + d[0] * r, y: self.y + d[1] * r, z: self.z + d[2] * r, size: FILE_STAR_SIZE });
      }
    }
  };

  place(0, null);

  // Center the constellation on its bounding box (the root star is usually off-center because
  // big subtrees pull one way), so the orbit ring and camera framing wrap what you actually see.
  let minX = Infinity, minY = Infinity, minZ = Infinity, maxX = -Infinity, maxY = -Infinity, maxZ = -Infinity;
  for (const p of nodes) {
    p.y *= Y_SQUASH;
    minX = Math.min(minX, p.x); maxX = Math.max(maxX, p.x);
    minY = Math.min(minY, p.y); maxY = Math.max(maxY, p.y);
    minZ = Math.min(minZ, p.z); maxZ = Math.max(maxZ, p.z);
  }
  const ox = (minX + maxX) / 2, oy = (minY + maxY) / 2, oz = (minZ + maxZ) / 2;
  let radius = 0;
  for (const p of nodes) {
    p.x -= ox; p.y -= oy; p.z -= oz;
    radius = Math.max(radius, Math.hypot(p.x, p.y, p.z) + p.size);
  }

  return { nodes, radius: Math.max(radius * 1.08, 6), fileCount: root.fileCount, folderCount: root.folderCount };
}

/**
 * Place repo constellations so they don't overlap: the first sits at the origin, later ones
 * walk outward along a golden-angle spiral until they find clear space. Input order should be
 * stable (add order), so adding a repo never moves the ones already there.
 *
 * Pass `previous` (key → center from the last placement) to keep constellations where they
 * are: a repo only moves if, after growing, it would overlap one placed before it.
 */
export function placeClusters(radii: readonly number[], keys: readonly string[], previous?: ReadonlyMap<string, readonly number[]>): V3[] {
  const centers: V3[] = [];
  const overlaps = (c: readonly number[], r: number, upTo: number): boolean => {
    for (let j = 0; j < upTo; j++) {
      const o = centers[j];
      const gap = 14 + r * 0.25 + radii[j] * 0.25;
      if (Math.hypot(c[0] - o[0], c[1] - o[1], c[2] - o[2]) < r + radii[j] + gap) return true;
    }
    return false;
  };
  for (let i = 0; i < radii.length; i++) {
    const r = radii[i];
    const kept = previous?.get(keys[i]);
    if (kept && !overlaps(kept, r, i)) { centers.push([kept[0], kept[1], kept[2]]); continue; }
    if (i === 0) { centers.push([0, 0, 0]); continue; }
    let placed: V3 | null = null;
    for (let step = 1; step < 4000 && !placed; step++) {
      const dist = Math.sqrt(step) * (r * 0.55 + 6);
      const angle = step * GOLDEN_ANGLE + hash01(keys[i], 11) * Math.PI * 2;
      const c: V3 = [Math.cos(angle) * dist, (hash01(keys[i], 12) - 0.5) * r * 0.2, Math.sin(angle) * dist];
      // Leave headroom when placing, so a repo can grow a bit later without having to move.
      if (!overlaps(c, r * 1.2 + 6, i)) placed = c;
    }
    centers.push(placed ?? [i * 200, 0, 0]);
  }
  return centers;
}
