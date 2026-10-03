/**
 * transition.ts — animate from one built scene to the next instead of jumping.
 *
 * Stars are matched across the two scenes by identity (repo id + path):
 *   - a star in both scenes glides from its old position/size to its new one
 *   - a new star grows out of its nearest ancestor that already existed (a brand-new repo
 *     blooms out of its core star), slightly later the deeper it sits below that ancestor
 *   - a removed star shrinks and fades back into its nearest surviving ancestor ("ghosts")
 * Lines follow the stars they connect, and new branches fade in with their child star.
 */
import { STAR_FLOATS, LINE_VERTEX_FLOATS } from '../../webgpu_core/index.js';
import { starKey } from './scene.js';
const DURATION_MS = 850;
const DEPTH_DELAY_MS = 70;
const MAX_DELAY_MS = 450;
const ease = (t) => (t <= 0 ? 0 : t >= 1 ? 1 : t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
export class SceneTransition {
    next;
    n;
    /** Per new-scene star: start position, start size, start "presence" (0 = not there yet), delay. */
    fromPos;
    fromSize;
    fromPresence;
    delay;
    /** Ghosts (removed stars): a copy of their old instance data plus where they collapse to. */
    ghosts;
    ghostTo;
    ghostCount;
    /** Per repo: old center minus new center, so rings glide with their constellation. */
    ringOffset;
    starBuf;
    lineBuf;
    curPos;
    curPresence;
    totalMs;
    constructor(prev, next) {
        this.next = next;
        const n = (this.n = next.starCount);
        const prevIndex = new Map();
        if (prev)
            for (let i = 0; i < prev.starCount; i++)
                prevIndex.set(starKey(prev, i), i);
        const nextIndex = new Map();
        for (let i = 0; i < n; i++)
            nextIndex.set(starKey(next, i), i);
        this.fromPos = new Float32Array(n * 3);
        this.fromSize = new Float32Array(n);
        this.fromPresence = new Float32Array(n);
        this.delay = new Float32Array(n);
        let maxDelay = 0;
        for (let i = 0; i < n; i++) {
            const old = prevIndex.get(starKey(next, i));
            if (prev && old !== undefined) {
                const po = old * STAR_FLOATS;
                this.fromPos.set(prev.stars.subarray(po, po + 3), i * 3);
                this.fromSize[i] = prev.stars[po + 3];
                this.fromPresence[i] = 1;
                continue;
            }
            // New star: start at the nearest ancestor that already existed (or this repo's core).
            const repo = next.repos[next.starRepo[i]];
            const nodes = repo.layout.nodes;
            const first = next.repoFirstStar[next.starRepo[i]];
            let a = nodes[next.starNode[i]].parent;
            let anchor = -1;
            let hops = 1;
            while (a >= 0) {
                const pi = prev ? prevIndex.get(`${repo.id}\u0000${nodes[a].node.path}`) : undefined;
                if (pi !== undefined && prev) {
                    anchor = pi;
                    break;
                }
                a = nodes[a].parent;
                hops++;
            }
            if (anchor >= 0 && prev) {
                const po = anchor * STAR_FLOATS;
                this.fromPos.set(prev.stars.subarray(po, po + 3), i * 3);
            }
            else {
                const ro = first * STAR_FLOATS; // bloom out of this constellation's core star
                this.fromPos.set(next.stars.subarray(ro, ro + 3), i * 3);
                hops = nodes[next.starNode[i]].node.depth;
            }
            this.fromSize[i] = 0;
            this.fromPresence[i] = 0;
            this.delay[i] = Math.min(MAX_DELAY_MS, Math.max(0, hops - 1) * DEPTH_DELAY_MS);
            maxDelay = Math.max(maxDelay, this.delay[i]);
        }
        // Ghosts: stars that disappeared, collapsing into their nearest surviving ancestor.
        const ghostIdx = [];
        if (prev)
            for (let i = 0; i < prev.starCount; i++)
                if (!nextIndex.has(starKey(prev, i)))
                    ghostIdx.push(i);
        this.ghostCount = ghostIdx.length;
        this.ghosts = new Float32Array(Math.max(1, ghostIdx.length) * STAR_FLOATS);
        this.ghostTo = new Float32Array(Math.max(1, ghostIdx.length) * 3);
        ghostIdx.forEach((pi, g) => {
            const po = pi * STAR_FLOATS;
            this.ghosts.set(prev.stars.subarray(po, po + STAR_FLOATS), g * STAR_FLOATS);
            this.ghosts[g * STAR_FLOATS + 8] = 0; // no hover/selection ring on a ghost
            const repo = prev.repos[prev.starRepo[pi]];
            let path = repo.layout.nodes[prev.starNode[pi]].node.path;
            let target = -1;
            while (target < 0 && path) {
                path = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
                target = nextIndex.get(`${repo.id}\u0000${path}`) ?? -1;
            }
            const src = target >= 0 ? next.stars.subarray(target * STAR_FLOATS, target * STAR_FLOATS + 3) : prev.stars.subarray(po, po + 3);
            this.ghostTo.set(src, g * 3);
        });
        this.ringOffset = new Float32Array(next.repos.length * 3);
        next.repos.forEach((r, ri) => {
            const old = prev?.repos.find((p) => p.id === r.id);
            if (!old)
                return;
            this.ringOffset[ri * 3] = old.center[0] - r.center[0];
            this.ringOffset[ri * 3 + 1] = old.center[1] - r.center[1];
            this.ringOffset[ri * 3 + 2] = old.center[2] - r.center[2];
        });
        this.starBuf = new Float32Array(Math.max(1, n + this.ghostCount) * STAR_FLOATS);
        this.lineBuf = new Float32Array(Math.max(1, next.lineVertexCount) * LINE_VERTEX_FLOATS);
        this.curPos = new Float32Array(n * 3);
        this.curPresence = new Float32Array(n);
        this.totalMs = DURATION_MS + maxDelay;
    }
    /** True when nothing would visibly change (e.g. a rescan that found the same files). */
    get isNoop() {
        if (this.ghostCount > 0)
            return false;
        for (let i = 0; i < this.n; i++) {
            if (this.fromPresence[i] < 1)
                return false;
            const o = i * STAR_FLOATS;
            const s = this.next.stars;
            if (Math.abs(this.fromPos[i * 3] - s[o]) > 1e-4 || Math.abs(this.fromPos[i * 3 + 1] - s[o + 1]) > 1e-4 ||
                Math.abs(this.fromPos[i * 3 + 2] - s[o + 2]) > 1e-4 || Math.abs(this.fromSize[i] - s[o + 3]) > 1e-4)
                return false;
        }
        return true;
    }
    frame(elapsedMs) {
        const next = this.next;
        const out = this.starBuf;
        const target = next.stars;
        for (let i = 0; i < this.n; i++) {
            const o = i * STAR_FLOATS;
            const k = ease((elapsedMs - this.delay[i]) / DURATION_MS);
            for (let f = 0; f < STAR_FLOATS; f++)
                out[o + f] = target[o + f]; // color, flags, etc.
            const x = this.fromPos[i * 3] + (target[o] - this.fromPos[i * 3]) * k;
            const y = this.fromPos[i * 3 + 1] + (target[o + 1] - this.fromPos[i * 3 + 1]) * k;
            const z = this.fromPos[i * 3 + 2] + (target[o + 2] - this.fromPos[i * 3 + 2]) * k;
            const presence = this.fromPresence[i] + (1 - this.fromPresence[i]) * k;
            out[o] = x;
            out[o + 1] = y;
            out[o + 2] = z;
            out[o + 3] = this.fromSize[i] + (target[o + 3] - this.fromSize[i]) * k;
            out[o + 7] = target[o + 7] * presence;
            out[o + 9] = target[o + 9] * presence; // minimum pixel size grows in too
            this.curPos[i * 3] = x;
            this.curPos[i * 3 + 1] = y;
            this.curPos[i * 3 + 2] = z;
            this.curPresence[i] = presence;
        }
        const gk = ease(elapsedMs / DURATION_MS);
        for (let g = 0; g < this.ghostCount; g++) {
            const o = (this.n + g) * STAR_FLOATS;
            const go = g * STAR_FLOATS;
            for (let f = 0; f < STAR_FLOATS; f++)
                out[o + f] = this.ghosts[go + f];
            for (let a = 0; a < 3; a++)
                out[o + a] = this.ghosts[go + a] + (this.ghostTo[g * 3 + a] - this.ghosts[go + a]) * gk;
            out[o + 3] = this.ghosts[go + 3] * (1 - gk);
            out[o + 7] = this.ghosts[go + 7] * (1 - gk);
            out[o + 9] = this.ghosts[go + 9] * (1 - gk);
        }
        const lines = this.lineBuf;
        const ringK = 1 - ease(elapsedMs / DURATION_MS);
        for (let v = 0; v < next.lineVertexCount; v++) {
            const o = v * LINE_VERTEX_FLOATS;
            for (let f = 0; f < LINE_VERTEX_FLOATS; f++)
                lines[o + f] = next.lines[o + f];
            const st = next.lineStar[v];
            if (st >= 0) {
                lines[o] = this.curPos[st * 3];
                lines[o + 1] = this.curPos[st * 3 + 1];
                lines[o + 2] = this.curPos[st * 3 + 2];
                lines[o + 6] *= this.curPresence[next.lineChild[v]];
            }
            else {
                const r = next.lineRepo[v] * 3;
                lines[o] += this.ringOffset[r] * ringK;
                lines[o + 1] += this.ringOffset[r + 1] * ringK;
                lines[o + 2] += this.ringOffset[r + 2] * ringK;
            }
        }
        return {
            stars: out,
            starCount: this.n + this.ghostCount,
            lines,
            lineVertexCount: next.lineVertexCount,
            done: elapsedMs >= this.totalMs,
        };
    }
}
