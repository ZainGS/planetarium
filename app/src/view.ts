/**
 * view.ts — the 3D constellation view: GPU passes, camera, picking, labels, fly-to.
 */

import {
  Camera3D, OrbitController, createGpuContext, syncCanvasSize, StarPass, LinePass, FrameLoop,
  pickNearest, projectToScreen, mat4, STAR_FLOATS, type GpuContext, type ScreenPoint,
} from '../../webgpu_core/index.js';
import { buildSkyStars, type BuiltScene } from './scene.js';
import { SceneTransition } from './transition.js';
import { AgentLayer } from './agent-layer.js';
import type { AgentInfo, Collision } from './api.js';
import { agentDisplayNames } from './api.js';
import { repoColorHex } from './palette.js';

export interface ViewCallbacks {
  onHover(star: number, cssX: number, cssY: number): void;
  onSelect(star: number): void;
  onHoverAgent(key: string | null, cssX: number, cssY: number): void;
  onSelectAgent(key: string): void;
}

const FOV = (50 * Math.PI) / 180;
const easeInOut = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);

export class ConstellationView {
  private readonly gpu: GpuContext;
  private readonly camera: Camera3D;
  private readonly orbit: OrbitController;
  private readonly loop: FrameLoop;
  private readonly stars: StarPass;
  private readonly sky: StarPass;
  private readonly lines: LinePass;
  private scene: BuiltScene | null = null;
  private hovered = -1;
  private selected = -1;
  private pointerDown = false;
  private downX = 0;
  private downY = 0;
  private downTime = 0;
  private lastClickTime = 0;
  private lastClickStar = -1;
  private pendingHover: { x: number; y: number } | null = null;
  private readonly skyView = mat4.create();
  private readonly skyProj = mat4.create();
  private readonly skyVP = mat4.create();
  private readonly labelEls: HTMLDivElement[] = [];
  private readonly folderLabelPool: HTMLDivElement[] = [];
  private readonly agentLabelPool: HTMLDivElement[] = [];
  /** Repos whose agents are spread over several branches/worktrees (their tags show the branch). */
  private branchyRepos = new Set<string>();
  private displayNames = new Map<string, string>();
  private readonly screenTmp: ScreenPoint = { x: 0, y: 0, depth: 0, visible: false };
  private flight: ((t: number, dt: number) => boolean) | null = null;
  private morph: ((t: number, dt: number) => boolean) | null = null;
  private readonly agentLayer: AgentLayer;
  private agentsAnimating = false;
  private hoveredAgent: string | null = null;

  private constructor(gpu: GpuContext, private readonly labelsEl: HTMLElement, private readonly cb: ViewCallbacks) {
    this.gpu = gpu;
    this.camera = new Camera3D({ position: [0, 40, 120], target: [0, 0, 0], fov: FOV, autoNear: true, autoFar: true, sceneRadius: 200 });
    this.orbit = new OrbitController(this.camera, {
      enableDamping: true, dampingFactor: 0.12, orbitSpeed: 0.0045, panSpeed: 0.0016, zoomSpeed: 0.12,
      minRadius: 1.5, maxRadius: 5000, minElevation: -1.45, maxElevation: 1.45,
    });
    this.orbit.onChange = () => this.loop.requestRender();
    this.orbit.attach(gpu.canvas);
    this.loop = new FrameLoop(() => this.render());
    this.stars = new StarPass(gpu.device, gpu.format, 'repo-stars');
    this.sky = new StarPass(gpu.device, gpu.format, 'sky-stars');
    this.lines = new LinePass(gpu.device, gpu.format, 'repo-lines');
    this.agentLayer = new AgentLayer(gpu.device, gpu.format);
    const skyData = buildSkyStars();
    this.sky.setInstances(skyData, skyData.length / STAR_FLOATS);
    this.bindInput();
    new ResizeObserver(() => { if (syncCanvasSize(this.gpu)) this.loop.requestRender(); }).observe(gpu.canvas);
    gpu.device.lost.then((info) => {
      if (info.reason !== 'destroyed') window.dispatchEvent(new CustomEvent('planetarium:gpu-lost', { detail: info.message }));
    });
  }

  static async create(canvas: HTMLCanvasElement, labelsEl: HTMLElement, cb: ViewCallbacks): Promise<ConstellationView> {
    const gpu = await createGpuContext(canvas);
    const view = new ConstellationView(gpu, labelsEl, cb);
    view.loop.requestRender();
    return view;
  }

  // ── Scene ────────────────────────────────────────────────────────

  /**
   * Show a new scene. With `animate`, stars that appeared grow out of their parents, removed ones
   * fold back in, and everything else glides to its new place (see transition.ts).
   */
  setScene(scene: BuiltScene, animate = true): void {
    const prevSelected = this.selected >= 0 && this.scene ? this.keyOf(this.selected) : null;
    const prev = this.scene;
    this.scene = scene;
    this.hovered = -1;
    this.selected = -1;
    if (this.morph) { this.loop.removeAnimator(this.morph); this.morph = null; }
    const transition = animate ? new SceneTransition(prev, scene) : null;
    if (transition && !transition.isNoop) {
      let elapsed = 0;
      const step = (_t: number, dt: number): boolean => {
        if (this.scene !== scene) return false; // superseded by a newer scene
        elapsed += dt;
        const f = transition.frame(elapsed);
        if (f.done) {
          this.stars.setInstances(scene.stars, scene.starCount);
          this.lines.setVertices(scene.lines, scene.lineVertexCount);
          this.morph = null;
          return false;
        }
        this.stars.setInstances(f.stars, f.starCount);
        this.lines.setVertices(f.lines, f.lineVertexCount);
        return true;
      };
      this.morph = step;
      step(0, 0);
      this.loop.addAnimator(step);
    } else {
      this.stars.setInstances(scene.stars, scene.starCount);
      this.lines.setVertices(scene.lines, scene.lineVertexCount);
    }
    this.agentLayer.setScene(scene);
    this.camera.sceneRadius = scene.extent;
    this.orbit.maxRadius = Math.max(400, scene.extent * 6);
    if (prevSelected) {
      const idx = this.findStar(prevSelected.repoId, prevSelected.path);
      if (idx >= 0) this.setSelected(idx);
    }
    this.rebuildLabels();
    this.loop.requestRender();
  }

  getScene(): BuiltScene | null { return this.scene; }

  // ── Agents ───────────────────────────────────────────────────────

  setAgents(list: AgentInfo[]): void {
    // Name tags carry the branch when agents in a repo are on more than one checkout or branch.
    const seen = new Map<string, Set<string>>();
    for (const a of list) {
      if (!a.repoId || a.status === 'done' || !a.branch) continue;
      const set = seen.get(a.repoId) ?? new Set<string>();
      set.add(`${a.treePath ?? ''}\u0000${a.branch}`);
      seen.set(a.repoId, set);
    }
    this.branchyRepos = new Set([...seen].filter(([, s]) => s.size > 1).map(([id]) => id));
    this.displayNames = agentDisplayNames(list);
    this.agentLayer.setAgents(list);
    this.ensureAgentAnimation();
  }

  setCollisions(list: Collision[]): void {
    this.agentLayer.setCollisions(list);
    this.ensureAgentAnimation();
  }

  /** Agents move continuously (trails, orbits), so frames keep flowing while any are visible. */
  private ensureAgentAnimation(): void {
    if (this.agentsAnimating) return;
    this.agentsAnimating = true;
    this.loop.addAnimator((t, dt) => {
      const still = this.agentLayer.update(t, dt);
      if (!still) this.agentsAnimating = false;
      return still;
    });
  }

  /** Stop drawing while nobody can see the window (tray / minimized). */
  setPaused(paused: boolean): void {
    this.loop.setPaused(paused);
  }

  selectAgent(key: string | null): void {
    this.agentLayer.setSelected(key);
    if (key) this.setSelected(-1);
    this.loop.requestRender();
  }

  flyToAgent(key: string): void {
    const p = this.agentLayer.positionOf(key);
    if (p) this.flyTo(p, 22);
  }

  agentScreenPosition(key: string): ScreenPoint | null {
    const p = this.agentLayer.positionOf(key);
    if (!p) return null;
    const rect = this.gpu.canvas.getBoundingClientRect();
    return projectToScreen(this.camera.getViewProjectionMatrix(), p[0], p[1], p[2], rect.width, rect.height);
  }

  private keyOf(star: number): { repoId: string; path: string } {
    const s = this.scene!;
    const repo = s.repos[s.starRepo[star]];
    return { repoId: repo.id, path: repo.layout.nodes[s.starNode[star]].node.path };
  }

  findStar(repoId: string, path: string): number {
    const s = this.scene;
    if (!s) return -1;
    const ri = s.repos.findIndex((r) => r.id === repoId);
    if (ri < 0) return -1;
    const ni = s.repos[ri].layout.nodes.findIndex((n) => n.node.path === path);
    return ni < 0 ? -1 : s.repoFirstStar[ri] + ni;
  }

  // ── Highlight state ──────────────────────────────────────────────

  private setHighlight(star: number, value: number): void {
    const s = this.scene;
    if (!s || star < 0 || star >= s.starCount) return;
    const o = star * STAR_FLOATS + 8;
    const v = star === this.selected ? 2 : value;
    if (s.stars[o] === v) return;
    s.stars[o] = v;
    // Mid-transition the next animation frame uploads the flag along with everything else.
    if (!this.morph) this.stars.updateInstance(star, s.stars);
  }

  setHovered(star: number): void {
    if (star === this.hovered) return;
    const prev = this.hovered;
    this.hovered = star;
    this.setHighlight(prev, 0);
    this.setHighlight(star, 1);
    this.gpu.canvas.style.cursor = star >= 0 ? 'pointer' : '';
    this.loop.requestRender();
  }

  setSelected(star: number): void {
    if (star === this.selected) return;
    const prev = this.selected;
    this.selected = star;
    this.setHighlight(prev, prev === this.hovered ? 1 : 0);
    this.setHighlight(star, 2);
    this.loop.requestRender();
  }

  get selectedStar(): number { return this.selected; }

  // ── Camera moves ─────────────────────────────────────────────────

  /** Smoothly move the orbit pivot to `target` and the orbit distance to `radius`. */
  flyTo(target: [number, number, number], radius: number, durationMs = 900): void {
    if (this.flight) this.loop.removeAnimator(this.flight);
    this.orbit.stopDamping();
    const from = [this.camera.target[0], this.camera.target[1], this.camera.target[2]];
    const fromR = this.orbit.radius;
    const toR = Math.min(Math.max(radius, this.orbit.minRadius), this.orbit.maxRadius);
    let elapsed = 0;
    const step = (_t: number, dt: number): boolean => {
      elapsed += dt;
      const k = easeInOut(Math.min(1, elapsed / durationMs));
      this.camera.setTarget(
        from[0] + (target[0] - from[0]) * k,
        from[1] + (target[1] - from[1]) * k,
        from[2] + (target[2] - from[2]) * k,
      );
      // Interpolate distance in log space so long zooms feel even.
      this.orbit.radius = Math.exp(Math.log(fromR) + (Math.log(toR) - Math.log(fromR)) * k);
      this.orbit.applySpherical();
      if (k >= 1) { this.flight = null; return false; }
      return true;
    };
    this.flight = step;
    this.loop.addAnimator(step);
  }

  private distanceToFit(radius: number): number {
    const aspect = this.gpu.width / Math.max(1, this.gpu.height);
    const halfFov = Math.min(FOV / 2, Math.atan(Math.tan(FOV / 2) * aspect));
    return (radius / Math.sin(halfFov)) * 1.05;
  }

  frameAll(animate = true): void {
    const s = this.scene;
    if (!s || s.repos.length === 0) {
      if (animate) this.flyTo([0, 0, 0], 120); else { this.camera.setTarget(0, 0, 0); this.orbit.radius = 120; this.orbit.applySpherical(); this.loop.requestRender(); }
      return;
    }
    let cx = 0, cy = 0, cz = 0;
    for (const r of s.repos) { cx += r.center[0]; cy += r.center[1]; cz += r.center[2]; }
    cx /= s.repos.length; cy /= s.repos.length; cz /= s.repos.length;
    let rad = 0;
    for (const r of s.repos) rad = Math.max(rad, Math.hypot(r.center[0] - cx, r.center[1] - cy, r.center[2] - cz) + r.layout.radius);
    const dist = this.distanceToFit(rad);
    if (animate) this.flyTo([cx, cy, cz], dist, 1100);
    else {
      this.camera.setTarget(cx, cy, cz);
      this.orbit.radius = dist;
      this.orbit.setSpherical(0.6, 0.42);
      this.loop.requestRender();
    }
  }

  flyToRepo(repoIndex: number): void {
    const s = this.scene;
    const r = s?.repos[repoIndex];
    if (!r) return;
    this.flyTo(r.center, this.distanceToFit(r.layout.radius * 0.95));
  }

  flyToStar(star: number): void {
    const s = this.scene;
    if (!s || star < 0) return;
    const o = star * STAR_FLOATS;
    const node = s.repos[s.starRepo[star]].layout.nodes[s.starNode[star]].node;
    const reach = node.isDir ? 6 + 2.2 * Math.sqrt(node.fileCount) : 6;
    this.flyTo([s.stars[o], s.stars[o + 1], s.stars[o + 2]], Math.min(reach, 160));
  }

  // ── Input ────────────────────────────────────────────────────────

  /** Agents take priority over stars when picking (they're small and sit right beside stars). */
  private cssToPickAgent(x: number, y: number): string | null {
    const { data, count, keys } = this.agentLayer.pickData();
    if (count === 0) return null;
    const rect = this.gpu.canvas.getBoundingClientRect();
    const vp = this.camera.getViewProjectionMatrix();
    const projScaleCss = (1 / Math.tan(FOV / 2)) * rect.height * 0.5;
    const i = pickNearest(vp, data, STAR_FLOATS, count, x, y, rect.width, rect.height, projScaleCss, 9);
    return i >= 0 ? keys[i] : null;
  }

  private setHoveredAgent(key: string | null): void {
    if (key === this.hoveredAgent) return;
    this.hoveredAgent = key;
    this.agentLayer.setHovered(key);
    this.gpu.canvas.style.cursor = key || this.hovered >= 0 ? 'pointer' : '';
    this.loop.requestRender();
  }

  private cssToPick(x: number, y: number): number {
    const s = this.scene;
    if (!s || s.starCount === 0) return -1;
    const rect = this.gpu.canvas.getBoundingClientRect();
    const vp = this.camera.getViewProjectionMatrix();
    const projScaleCss = (1 / Math.tan(FOV / 2)) * rect.height * 0.5;
    return pickNearest(vp, s.stars, STAR_FLOATS, s.starCount, x, y, rect.width, rect.height, projScaleCss, 5);
  }

  private bindInput(): void {
    const canvas = this.gpu.canvas;
    canvas.addEventListener('contextmenu', (e) => e.preventDefault());

    // Keep frames flowing while dragging and while the orbit damping settles.
    const orbitAnimator = (): boolean => this.orbit.update() || this.pointerDown;

    canvas.addEventListener('pointerdown', (e) => {
      this.pointerDown = true;
      this.downX = e.clientX; this.downY = e.clientY; this.downTime = performance.now();
      canvas.setPointerCapture(e.pointerId);
      if (this.flight) { this.loop.removeAnimator(this.flight); this.flight = null; }
      this.loop.addAnimator(orbitAnimator);
    });

    canvas.addEventListener('pointerup', (e) => {
      this.pointerDown = false;
      const moved = Math.hypot(e.clientX - this.downX, e.clientY - this.downY);
      if (e.button === 0 && moved < 4 && performance.now() - this.downTime < 500) {
        const rect = canvas.getBoundingClientRect();
        const agent = this.cssToPickAgent(e.clientX - rect.left, e.clientY - rect.top);
        if (agent) {
          this.agentLayer.setSelected(agent);
          this.setSelected(-1);
          this.cb.onSelectAgent(agent);
          this.loop.requestRender();
          return;
        }
        this.agentLayer.setSelected(null);
        const star = this.cssToPick(e.clientX - rect.left, e.clientY - rect.top);
        const now = performance.now();
        if (star >= 0 && star === this.lastClickStar && now - this.lastClickTime < 400) {
          this.flyToStar(star);
        }
        this.lastClickStar = star;
        this.lastClickTime = now;
        this.setSelected(star);
        this.cb.onSelect(star);
      }
    });

    canvas.addEventListener('pointermove', (e) => {
      if (this.pointerDown) { this.setHovered(-1); this.cb.onHover(-1, 0, 0); return; }
      const rect = canvas.getBoundingClientRect();
      const first = this.pendingHover === null;
      this.pendingHover = { x: e.clientX - rect.left, y: e.clientY - rect.top };
      if (first) {
        requestAnimationFrame(() => {
          const p = this.pendingHover;
          this.pendingHover = null;
          if (!p || this.pointerDown) return;
          const agent = this.cssToPickAgent(p.x, p.y);
          this.setHoveredAgent(agent);
          if (agent) {
            this.setHovered(-1);
            this.cb.onHoverAgent(agent, p.x, p.y);
            return;
          }
          this.cb.onHoverAgent(null, 0, 0);
          const star = this.cssToPick(p.x, p.y);
          this.setHovered(star);
          this.cb.onHover(star, p.x, p.y);
        });
      }
    });

    canvas.addEventListener('pointerleave', () => {
      this.pendingHover = null;
      this.setHovered(-1);
      this.cb.onHover(-1, 0, 0);
    });

    canvas.addEventListener('wheel', () => { this.setHovered(-1); this.cb.onHover(-1, 0, 0); }, { passive: true });
  }

  // ── Labels (HTML overlay) ────────────────────────────────────────

  private rebuildLabels(): void {
    for (const el of this.labelEls) el.remove();
    this.labelEls.length = 0;
    const s = this.scene;
    if (!s) return;
    for (const r of s.repos) {
      const el = document.createElement('div');
      el.className = 'label repo-label';
      el.style.setProperty('--repo-color', repoColorHex(r.colorIndex));
      const name = document.createElement('span');
      name.className = 'name';
      name.textContent = r.name;
      const meta = document.createElement('span');
      meta.className = 'meta';
      meta.textContent = `${r.layout.fileCount.toLocaleString()} files`;
      el.append(name, meta);
      this.labelsEl.appendChild(el);
      this.labelEls.push(el);
    }
  }

  private folderLabel(i: number): HTMLDivElement {
    let el = this.folderLabelPool[i];
    if (!el) {
      el = document.createElement('div');
      el.className = 'label folder-label';
      this.labelsEl.appendChild(el);
      this.folderLabelPool[i] = el;
    }
    return el;
  }

  private updateLabels(vp: Float32Array): void {
    const s = this.scene;
    const rect = this.gpu.canvas.getBoundingClientRect();
    const w = rect.width, h = rect.height;
    const projScaleCss = (1 / Math.tan(FOV / 2)) * h * 0.5;
    let folderUsed = 0;
    if (s) {
      // Repo labels: place nearest first, and nudge a label upward if it would cover one already placed.
      const placed: { x0: number; y0: number; x1: number; y1: number }[] = [];
      const order: { i: number; x: number; y: number; depth: number }[] = [];
      s.repos.forEach((r, i) => {
        const el = this.labelEls[i];
        const p = projectToScreen(vp, r.center[0], r.center[1] + r.layout.radius * 0.45, r.center[2], w, h, this.screenTmp);
        if (!p.visible) { el.style.opacity = '0'; return; }
        const screenRadius = r.layout.radius * projScaleCss / p.depth;
        order.push({ i, x: p.x, y: p.y - Math.min(screenRadius * 0.15, 40) - 14, depth: p.depth });

        // Close enough to read the structure: name the top-level folders.
        if (screenRadius > 240) {
          const nodes = r.layout.nodes;
          const fade = Math.min(1, (screenRadius - 240) / 160);
          for (let ni = 1; ni < nodes.length && folderUsed < 80; ni++) {
            const n = nodes[ni];
            if (!n.node.isDir || n.parent !== 0) continue;
            const q = projectToScreen(vp, r.center[0] + n.x, r.center[1] + n.y, r.center[2] + n.z, w, h, this.screenTmp);
            if (!q.visible) continue;
            const fl = this.folderLabel(folderUsed++);
            if (fl.textContent !== n.node.name) fl.textContent = n.node.name;
            fl.style.opacity = String(fade * 0.85);
            fl.style.transform = `translate(${(q.x + 8).toFixed(1)}px, ${(q.y - 8).toFixed(1)}px)`;
          }
        }
      });
      order.sort((a, b) => a.depth - b.depth);
      for (const o of order) {
        const el = this.labelEls[o.i];
        const lw = el.offsetWidth, lh = el.offsetHeight;
        let x0 = o.x - lw / 2, y1 = o.y;
        for (let guard = 0; guard < 8; guard++) {
          const hit = placed.find((r) => x0 < r.x1 && x0 + lw > r.x0 && y1 - lh < r.y1 && y1 > r.y0);
          if (!hit) break;
          y1 = hit.y0 - 2;
        }
        placed.push({ x0, y0: y1 - lh, x1: x0 + lw, y1 });
        el.style.opacity = '1';
        el.style.transform = `translate(${x0.toFixed(1)}px, ${(y1 - lh).toFixed(1)}px)`;
      }
    }
    for (let i = folderUsed; i < this.folderLabelPool.length; i++) this.folderLabelPool[i].style.opacity = '0';

    // Agent name tags, beside each agent dot.
    let agentUsed = 0;
    for (const l of this.agentLayer.labels()) {
      const q = projectToScreen(vp, l.pos[0], l.pos[1], l.pos[2], w, h, this.screenTmp);
      if (!q.visible) continue;
      let el = this.agentLabelPool[agentUsed];
      if (!el) {
        el = document.createElement('div');
        el.className = 'label agent-label';
        this.labelsEl.appendChild(el);
        this.agentLabelPool[agentUsed] = el;
      }
      agentUsed++;
      const base = this.displayNames.get(l.info.key) ?? (l.info.parentKey ? l.info.agentType : (l.info.title || `Claude · ${l.info.sessionId.slice(0, 4)}`));
      const showBranch = l.info.branch && (l.info.worktree || (l.info.repoId && this.branchyRepos.has(l.info.repoId)));
      const name = showBranch ? `${base} · ${l.info.branch}` : base;
      // Subagents carry what they're doing on a second line.
      const desc = l.info.parentKey ? (l.info.description ?? '') : '';
      const sig = `${name}\u0000${desc}`;
      if (el.dataset.sig !== sig) {
        el.dataset.sig = sig;
        const top = document.createElement('span');
        top.textContent = name;
        if (desc) {
          const sub = document.createElement('span');
          sub.className = 'desc';
          sub.textContent = desc;
          el.replaceChildren(top, sub);
        } else {
          el.replaceChildren(top);
        }
        el.classList.toggle('two-line', !!desc);
      }
      const sw = l.info.branchSwitch;
      el.classList.toggle('switched', !!sw && Date.now() - sw.at < 5 * 60_000);
      el.style.setProperty('--agent-color', `rgb(${Math.round(l.color[0] * 255)}, ${Math.round(l.color[1] * 255)}, ${Math.round(l.color[2] * 255)})`);
      el.classList.toggle('sub', !!l.info.parentKey);
      el.style.opacity = String(Math.min(1, l.presence) * (l.info.status === 'working' ? 1 : 0.7));
      el.style.transform = `translate(${(q.x + 12).toFixed(1)}px, ${(q.y - 8).toFixed(1)}px)`;
    }
    for (let i = agentUsed; i < this.agentLabelPool.length; i++) this.agentLabelPool[i].style.opacity = '0';
  }

  /** Screen position (CSS px) of a star, for anchoring the tooltip after camera moves. */
  starScreenPosition(star: number): ScreenPoint | null {
    const s = this.scene;
    if (!s || star < 0) return null;
    const rect = this.gpu.canvas.getBoundingClientRect();
    const o = star * STAR_FLOATS;
    return projectToScreen(this.camera.getViewProjectionMatrix(), s.stars[o], s.stars[o + 1], s.stars[o + 2], rect.width, rect.height);
  }

  // ── Frame ────────────────────────────────────────────────────────

  requestRender(): void { this.loop.requestRender(); }

  private render(): void {
    const gpu = this.gpu;
    syncCanvasSize(gpu);
    const w = gpu.width, h = gpu.height;
    this.camera.aspect = w / h;
    const vp = this.camera.getViewProjectionMatrix();
    const projScale = (1 / Math.tan(FOV / 2)) * h * 0.5;

    // Sky: same orientation as the camera, no translation, so it never gets closer.
    const t = this.camera.target, p = this.camera.position;
    mat4.lookAt(this.skyView, [0, 0, 0], [t[0] - p[0], t[1] - p[1], t[2] - p[2]], [0, 1, 0]);
    mat4.perspectiveZO(this.skyProj, FOV, w / h, 0.1, 500);
    mat4.mul(this.skyVP, this.skyProj, this.skyView);

    const encoder = gpu.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [{
        view: gpu.context.getCurrentTexture().createView(),
        clearValue: { r: 0, g: 0, b: 0, a: 0 },
        loadOp: 'clear',
        storeOp: 'store',
      }],
    });
    this.sky.draw(pass, this.skyVP, w, h, projScale);
    this.lines.draw(pass, vp);
    this.stars.draw(pass, vp, w, h, projScale);
    this.agentLayer.draw(pass, vp, w, h, projScale);
    pass.end();
    gpu.device.queue.submit([encoder.finish()]);

    this.updateLabels(vp);
    window.dispatchEvent(new CustomEvent('planetarium:frame'));
  }
}
