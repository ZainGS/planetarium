/**
 * agent-layer.ts — coding agents drawn into the constellation.
 *
 * Each agent is a bright dot in its repo's color:
 *   working  flies to the star of the file it's touching and hovers there, leaving a fading
 *            trail; an edit makes it flare
 *   idle     drifts out to the repo's orbit ring and slowly circles it
 *   waiting  (a main agent whose subagents are still busy) circles the ring too, breathing
 *   done     fades out
 * Subagents are smaller and keep a faint tether to their parent agent.
 */

import { StarPass, LinePass, STAR_FLOATS, LINE_VERTEX_FLOATS } from '../../webgpu_core/index.js';
import type { AgentInfo, Collision } from './api.js';
import { isOpenTalk } from './api.js';
import type { BuiltScene } from './scene.js';
import { hash01 } from './layout.js';
import { repoColor, tint, type RGB } from './palette.js';

const TRAIL_POINTS = 36;
const TRAIL_SAMPLE_MS = 28;
const ORBIT_PERIOD_S = 140; // one lap of the ring
const FOLLOW_RATE = 3.2; // higher = snappier movement toward the target

interface Visual {
  info: AgentInfo;
  pos: [number, number, number];
  placed: boolean;
  presence: number;
  pulse: number;
  lastEdits: number;
  trail: Float32Array; // TRAIL_POINTS * 3, ring buffer
  trailHead: number;
  trailCount: number;
  trailClock: number;
  orbitPhase: number;
  color: RGB;
  alive: boolean;
}

export interface AgentScreenInfo { key: string; x: number; y: number; z: number }

export class AgentLayer {
  private readonly stars: StarPass;
  private readonly lines: LinePass;
  private readonly visuals = new Map<string, Visual>();
  private starBuf = new Float32Array(16 * STAR_FLOATS);
  private lineBuf = new Float32Array(256 * LINE_VERTEX_FLOATS);
  private drawnKeys: string[] = [];
  private scene: BuiltScene | null = null;
  /** repoId → (lower-cased path → star index), for finding the star an agent is on. */
  private starIndex = new Map<string, Map<string, number>>();
  private hoveredKey: string | null = null;
  private collisions: Collision[] = [];
  private selectedKey: string | null = null;

  constructor(device: GPUDevice, format: GPUTextureFormat) {
    this.stars = new StarPass(device, format, 'agent-stars');
    this.lines = new LinePass(device, format, 'agent-lines');
  }

  get hasVisibleAgents(): boolean {
    if (this.collisions.some((c) => c.status === 'waiting' || isOpenTalk(c))) return true;
    for (const v of this.visuals.values()) if (v.alive || v.presence > 0.01) return true;
    return false;
  }

  setScene(scene: BuiltScene): void {
    this.scene = scene;
    this.starIndex.clear();
    scene.repos.forEach((r, ri) => {
      const m = new Map<string, number>();
      const first = scene.repoFirstStar[ri];
      r.layout.nodes.forEach((n, ni) => m.set(n.node.path.toLowerCase(), first + ni));
      this.starIndex.set(r.id, m);
    });
    for (const v of this.visuals.values()) v.color = this.colorFor(v.info);
  }

  setAgents(list: AgentInfo[]): void {
    const seen = new Set<string>();
    for (const info of list) {
      seen.add(info.key);
      const v = this.visuals.get(info.key);
      if (v) {
        if (info.edits > v.lastEdits) v.pulse = 1;
        v.lastEdits = info.edits;
        v.info = info;
        v.alive = info.status !== 'done';
        v.color = this.colorFor(info);
      } else {
        this.visuals.set(info.key, {
          info,
          pos: [0, 0, 0],
          placed: false,
          presence: 0,
          pulse: 0,
          lastEdits: info.edits,
          trail: new Float32Array(TRAIL_POINTS * 3),
          trailHead: 0,
          trailCount: 0,
          trailClock: 0,
          orbitPhase: hash01(info.key, 21) * Math.PI * 2,
          color: this.colorFor(info),
          alive: info.status !== 'done',
        });
      }
    }
    for (const [key, v] of this.visuals) if (!seen.has(key)) v.alive = false;
  }

  setHovered(key: string | null): void { this.hoveredKey = key; }

  /** Recent collisions, drawn as amber flares on the file and links between the agents involved. */
  setCollisions(list: Collision[]): void { this.collisions = list; }

  private starPos(repoId: string, file: string): [number, number, number] | null {
    const star = this.starForFile(repoId, file);
    if (star < 0 || !this.scene) return null;
    const o = star * STAR_FLOATS;
    return [this.scene.stars[o], this.scene.stars[o + 1], this.scene.stars[o + 2]];
  }
  setSelected(key: string | null): void { this.selectedKey = key; }

  private colorFor(info: AgentInfo): RGB {
    const ri = this.scene?.repos.findIndex((r) => r.id === info.repoId) ?? -1;
    if (ri < 0 || !this.scene) return [0.85, 0.88, 1];
    return tint(repoColor(this.scene.repos[ri].colorIndex), 0.3);
  }

  private repoOf(info: AgentInfo) {
    if (!this.scene || !info.repoId) return null;
    const ri = this.scene.repos.findIndex((r) => r.id === info.repoId);
    return ri < 0 ? null : { repo: this.scene.repos[ri], ri };
  }

  /** The star an agent's file maps to: the file itself, else its nearest listed folder, else the root. */
  private starForFile(repoId: string, file: string | null): number {
    const idx = this.starIndex.get(repoId);
    if (!idx) return -1;
    let p = (file ?? '').toLowerCase();
    while (true) {
      const hit = idx.get(p);
      if (hit !== undefined) return hit;
      if (!p) return -1;
      p = p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '';
    }
  }

  private target(v: Visual, timeS: number, slot: number, slots: number): [number, number, number] | null {
    const scene = this.scene;
    const info = v.info;
    const found = this.repoOf(info);
    if (!scene || !found) return null;
    const { repo } = found;
    const onRing = info.status !== 'working' || !info.file;
    if (onRing) {
      const rr = repo.layout.radius * 1.04;
      const speed = (Math.PI * 2) / ORBIT_PERIOD_S;
      const a = v.orbitPhase + timeS * speed * (info.parentKey ? 1.25 : 1);
      const bob = Math.sin(timeS * 0.9 + v.orbitPhase * 3) * repo.layout.radius * 0.012;
      return [repo.center[0] + Math.cos(a) * rr, repo.center[1] + bob, repo.center[2] + Math.sin(a) * rr];
    }
    const star = this.starForFile(repo.id, info.file);
    if (star < 0) return [repo.center[0], repo.center[1], repo.center[2]];
    const o = star * STAR_FLOATS;
    // Hover just beside the star; agents sharing a star spread around it.
    const r = 0.9 + 0.25 * Math.min(slots, 6);
    const a = (slot / Math.max(1, slots)) * Math.PI * 2 + timeS * 0.6 + v.orbitPhase;
    return [
      scene.stars[o] + Math.cos(a) * r,
      scene.stars[o + 1] + 0.55 + Math.sin(timeS * 1.7 + v.orbitPhase) * 0.12,
      scene.stars[o + 2] + Math.sin(a) * r,
    ];
  }

  /** Advance the simulation. Returns true while anything is still moving or visible. */
  update(timeMs: number, dtMs: number): boolean {
    const dt = Math.min(dtMs, 100) / 1000;
    const timeS = timeMs / 1000;

    // Group working agents by the star they're on, so they can share it gracefully.
    const groups = new Map<string, string[]>();
    for (const v of this.visuals.values()) {
      if (v.info.status === 'working' && v.info.file && v.info.repoId) {
        const k = `${v.info.repoId}|${v.info.file.toLowerCase()}`;
        (groups.get(k) ?? groups.set(k, []).get(k)!).push(v.info.key);
      }
    }

    let active = false;
    for (const [key, v] of this.visuals) {
      const gk = v.info.repoId && v.info.file ? `${v.info.repoId}|${v.info.file.toLowerCase()}` : '';
      const group = groups.get(gk);
      const slot = group ? group.indexOf(key) : 0;
      const target = this.target(v, timeS, slot, group ? group.length : 1);

      const wantPresence = v.alive && target ? 1 : 0;
      const fadeRate = wantPresence ? 2.2 : 0.55;
      v.presence += (wantPresence - v.presence) * (1 - Math.exp(-dt * fadeRate * 3));
      if (!v.alive && v.presence < 0.01) { this.visuals.delete(key); continue; }
      active = true;
      if (!target) continue;

      if (!v.placed) {
        // First appearance: start at the repo's core so it visibly launches out to its file.
        const found = this.repoOf(v.info);
        v.pos = found ? [found.repo.center[0], found.repo.center[1], found.repo.center[2]] : [...target];
        v.placed = true;
      }
      const k = 1 - Math.exp(-dt * FOLLOW_RATE);
      v.pos[0] += (target[0] - v.pos[0]) * k;
      v.pos[1] += (target[1] - v.pos[1]) * k;
      v.pos[2] += (target[2] - v.pos[2]) * k;
      v.pulse = Math.max(0, v.pulse - dt * 1.4);

      v.trailClock += dtMs;
      if (v.trailClock >= TRAIL_SAMPLE_MS) {
        v.trailClock = 0;
        v.trail.set(v.pos, v.trailHead * 3);
        v.trailHead = (v.trailHead + 1) % TRAIL_POINTS;
        v.trailCount = Math.min(TRAIL_POINTS, v.trailCount + 1);
      }
    }
    this.fillBuffers(timeS);
    return active || this.collisions.length > 0;
  }

  private fillBuffers(timeS: number): void {
    const list = [...this.visuals.values()].filter((v) => v.placed);
    const holdCount = list.reduce((n, v) => n + (v.alive ? v.info.holds.length : 0), 0);
    const needStars = (list.length + holdCount + this.collisions.length) * STAR_FLOATS;
    if (this.starBuf.length < needStars) this.starBuf = new Float32Array(needStars * 2);
    const needLines = (list.length * (TRAIL_POINTS * 2 + 2) + holdCount * 2 + this.collisions.length * 8) * LINE_VERTEX_FLOATS;
    if (this.lineBuf.length < needLines) this.lineBuf = new Float32Array(needLines * 2);

    const sb = this.starBuf;
    const lb = this.lineBuf;
    let l = 0;
    const line = (x: number, y: number, z: number, c: RGB, a: number) => {
      const o = l * LINE_VERTEX_FLOATS;
      lb[o] = x; lb[o + 1] = y; lb[o + 2] = z; lb[o + 3] = c[0]; lb[o + 4] = c[1]; lb[o + 5] = c[2]; lb[o + 6] = a;
      l++;
    };

    this.drawnKeys = list.map((v) => v.info.key);
    list.forEach((v, i) => {
      const o = i * STAR_FLOATS;
      const sub = !!v.info.parentKey;
      const waiting = v.info.status === 'waiting';
      const breathe = waiting ? 0.75 + 0.25 * Math.sin(timeS * 2.2 + v.orbitPhase) : 1;
      // Agents are markers, not scenery: a fixed on-screen size (via the min-pixel floor),
      // so they read the same whether you're zoomed in on a file or looking at every repo.
      const size = 0.001;
      const px = (sub ? 6.5 : 9) * (1 + v.pulse * 0.8);
      const idleDim = v.info.status === 'working' ? 1 : 0.7;
      sb[o] = v.pos[0]; sb[o + 1] = v.pos[1]; sb[o + 2] = v.pos[2]; sb[o + 3] = size;
      const c = tint(v.color, 0.25 + v.pulse * 0.5);
      sb[o + 4] = c[0]; sb[o + 5] = c[1]; sb[o + 6] = c[2];
      sb[o + 7] = v.presence * breathe * idleDim * (1 + v.pulse * 0.6);
      // Agents always wear a ring (1); selected ones a brighter one (2). It sets them apart from stars.
      sb[o + 8] = v.info.key === this.selectedKey || v.info.key === this.hoveredKey ? 2 : 1;
      sb[o + 9] = px * (0.4 + 0.6 * v.presence);
      sb[o + 10] = 0.42;
      sb[o + 11] = 0;

      // Fading trail, newest point first.
      for (let t = 0; t + 1 < v.trailCount; t++) {
        const a = (v.trailHead - 1 - t + TRAIL_POINTS) % TRAIL_POINTS;
        const b = (v.trailHead - 2 - t + TRAIL_POINTS) % TRAIL_POINTS;
        const fa = (1 - t / TRAIL_POINTS) * 0.85 * v.presence;
        const fb = (1 - (t + 1) / TRAIL_POINTS) * 0.85 * v.presence;
        line(v.trail[a * 3], v.trail[a * 3 + 1], v.trail[a * 3 + 2], v.color, fa);
        line(v.trail[b * 3], v.trail[b * 3 + 1], v.trail[b * 3 + 2], v.color, fb);
      }

      // Tether from a subagent to its parent: clear while it's working, barely there while idle.
      if (v.info.parentKey) {
        const parent = this.visuals.get(v.info.parentKey);
        if (parent?.placed) {
          const focus = v.info.key === this.selectedKey || v.info.key === this.hoveredKey;
          const strength = v.info.status === 'working' || focus ? 0.32 : 0.07;
          const a = strength * Math.min(v.presence, parent.presence);
          line(v.pos[0], v.pos[1], v.pos[2], v.color, a);
          line(parent.pos[0], parent.pos[1], parent.pos[2], parent.color, a * 0.6);
        }
      }
    });

    // ── Held files: a soft halo in the holder's color. The thread back to the holder is only
    //    drawn while it's working or when you point at it, so idle agents don't clutter the view.
    let n = list.length;
    const star = (x: number, y: number, z: number, c: RGB, alpha: number, minPx: number, core: number, ring: number) => {
      const o = n * STAR_FLOATS;
      sb[o] = x; sb[o + 1] = y; sb[o + 2] = z; sb[o + 3] = 0.001;
      sb[o + 4] = c[0]; sb[o + 5] = c[1]; sb[o + 6] = c[2]; sb[o + 7] = alpha;
      sb[o + 8] = ring; sb[o + 9] = minPx; sb[o + 10] = core; sb[o + 11] = 0;
      n++;
    };
    for (const v of list) {
      if (!v.alive) continue;
      const focus = v.info.key === this.selectedKey || v.info.key === this.hoveredKey;
      const threads = v.info.status === 'working' || focus;
      for (const h of v.info.holds) {
        const p = this.starPos(h.repoId, h.file);
        if (!p) continue;
        const breathe = 0.8 + 0.2 * Math.sin(timeS * 1.6 + v.orbitPhase);
        star(p[0], p[1], p[2], v.color, (threads ? 0.42 : 0.3) * v.presence * breathe, 16, 0.05, 0);
        if (threads) {
          line(p[0], p[1], p[2], v.color, (focus ? 0.3 : 0.16) * v.presence);
          line(v.pos[0], v.pos[1], v.pos[2], v.color, (focus ? 0.12 : 0.05) * v.presence);
        }
      }
    }

    // ── Collisions: amber flare on the file, amber links between the agents involved ──
    const amber: RGB = [1.0, 0.68, 0.22];
    for (const c of this.collisions) {
      const p = this.starPos(c.repoId, c.file);
      if (!p) continue;
      // Still open: waiting on you, or agents working it out between themselves.
      const open = c.status === 'waiting' || isOpenTalk(c);
      const live = open || c.status === 'warned' || c.status === 'proceeded' || c.status === 'agreed';
      const age = (Date.now() - (c.resolvedAt ?? c.createdAt)) / 1000;
      const fade = open ? 1 : Math.max(0, 1 - age / 40);
      if (fade <= 0) continue;
      // Waiting on you pulses fast; agents talking it out pulses slowly.
      const pulse = c.status === 'waiting' || c.status === 'needs_you'
        ? 0.65 + 0.35 * Math.sin(timeS * 5)
        : open ? 0.7 + 0.3 * Math.sin(timeS * 2) : 0.8;
      star(p[0], p[1], p[2], amber, (live ? 0.9 : 0.5) * fade * pulse, open ? 22 : 15, 0.12, 1);
      const incoming = this.visuals.get(c.agentKey);
      for (const hk of c.holderKeys) {
        const holder = this.visuals.get(hk);
        if (incoming?.placed && holder?.placed) {
          line(incoming.pos[0], incoming.pos[1], incoming.pos[2], amber, 0.65 * fade);
          line(holder.pos[0], holder.pos[1], holder.pos[2], amber, 0.65 * fade);
        }
      }
      if (incoming?.placed) {
        line(incoming.pos[0], incoming.pos[1], incoming.pos[2], amber, 0.45 * fade);
        line(p[0], p[1], p[2], amber, 0.45 * fade);
      }
    }

    this.stars.setInstances(sb, n);
    this.lines.setVertices(lb, l);
  }

  draw(pass: GPURenderPassEncoder, viewProj: ArrayLike<number>, width: number, height: number, projScale: number): void {
    this.lines.draw(pass, viewProj);
    this.stars.draw(pass, viewProj, width, height, projScale);
  }

  /** Data for picking: positions/sizes in star layout, and which agent each instance is. */
  pickData(): { data: Float32Array; count: number; keys: string[] } {
    return { data: this.starBuf, count: this.drawnKeys.length, keys: this.drawnKeys };
  }

  /** What the label overlay needs for each visible agent. */
  labels(): { info: AgentInfo; pos: [number, number, number]; presence: number; color: RGB }[] {
    const out = [];
    for (const v of this.visuals.values()) {
      if (v.placed && v.presence > 0.05) out.push({ info: v.info, pos: v.pos, presence: v.presence, color: v.color });
    }
    return out;
  }

  positionOf(key: string): [number, number, number] | null {
    const v = this.visuals.get(key);
    return v?.placed ? [v.pos[0], v.pos[1], v.pos[2]] : null;
  }
}
