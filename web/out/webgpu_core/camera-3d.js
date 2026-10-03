/**
 * Camera3D — Perspective and orthographic camera for 3D rendering.
 *
 * Produces a viewProjection mat4 that can be fed to the renderer as the
 * "world matrix" uniform. When in orthographic mode with default settings,
 * it produces output compatible with the existing 2D pipeline.
 *
 * Fully self-contained — no dependencies on the 2D renderer.
 */
import { mat4, vec3 } from './math.js';
export class Camera3D {
    _position;
    _target;
    _up;
    _fov;
    _near;
    _far;
    _aspect = 1;
    _orthoSize;
    _orthoOffsetX = 0;
    _orthoOffsetY = 0;
    _mode;
    _autoNear;
    _autoFar;
    _sceneRadius;
    // Cached matrices — recomputed on demand
    _viewDirty = true;
    _projDirty = true;
    _viewMatrix = mat4.create();
    _projMatrix = mat4.create();
    _vpMatrix = mat4.create();
    _vpDirty = true;
    constructor(config = {}) {
        this._position = vec3.fromValues(...(config.position ?? [0, 0, 3]));
        this._target = vec3.fromValues(...(config.target ?? [0, 0, 0]));
        this._up = vec3.fromValues(...(config.up ?? [0, 1, 0]));
        this._fov = config.fov ?? (Math.PI / 4); // 45°
        this._near = config.near ?? 0.01;
        this._far = config.far ?? 100;
        this._orthoSize = config.orthoSize ?? 1;
        this._mode = config.mode ?? 'perspective';
        this._autoNear = config.autoNear ?? false;
        this._autoFar = config.autoFar ?? false;
        this._sceneRadius = config.sceneRadius ?? 0;
    }
    // ── Getters / Setters ──────────────────────────────────────────
    get position() { return this._position; }
    set position(v) { vec3.copy(this._position, v); this.markViewDirty(); }
    get target() { return this._target; }
    set target(v) { vec3.copy(this._target, v); this.markViewDirty(); }
    get up() { return this._up; }
    set up(v) { vec3.copy(this._up, v); this.markViewDirty(); }
    get fov() { return this._fov; }
    set fov(v) { this._fov = v; this.markProjDirty(); }
    get near() { return this._near; }
    set near(v) { this._near = v; this.markProjDirty(); }
    get far() { return this._far; }
    set far(v) { this._far = v; this.markProjDirty(); }
    get autoFar() { return this._autoFar; }
    set autoFar(v) { this._autoFar = v; this.markProjDirty(); }
    get sceneRadius() { return this._sceneRadius; }
    set sceneRadius(v) { this._sceneRadius = v; if (this._autoFar)
        this.markProjDirty(); }
    /** The far plane actually used this frame (tracks camera distance + scene radius when autoFar is on). */
    get effectiveFar() {
        return this._autoFar && this._mode === 'perspective'
            ? vec3.distance(this._position, this._target) + this._sceneRadius * 2 + 1
            : this._far;
    }
    get aspect() { return this._aspect; }
    set aspect(v) { this._aspect = v; this.markProjDirty(); }
    get orthoSize() { return this._orthoSize; }
    set orthoSize(v) { this._orthoSize = v; this.markProjDirty(); }
    get orthoOffsetX() { return this._orthoOffsetX; }
    set orthoOffsetX(v) { this._orthoOffsetX = v; this.markProjDirty(); }
    get orthoOffsetY() { return this._orthoOffsetY; }
    set orthoOffsetY(v) { this._orthoOffsetY = v; this.markProjDirty(); }
    get mode() { return this._mode; }
    set mode(v) { this._mode = v; this.markProjDirty(); }
    get autoNear() { return this._autoNear; }
    set autoNear(v) { this._autoNear = v; this.markProjDirty(); }
    // ── Convenience setters ────────────────────────────────────────
    setPosition(x, y, z) {
        vec3.set(this._position, x, y, z);
        this.markViewDirty();
    }
    setTarget(x, y, z) {
        vec3.set(this._target, x, y, z);
        this.markViewDirty();
    }
    lookAt(eyeX, eyeY, eyeZ, tgtX, tgtY, tgtZ) {
        vec3.set(this._position, eyeX, eyeY, eyeZ);
        vec3.set(this._target, tgtX, tgtY, tgtZ);
        this.markViewDirty();
    }
    // ── Matrix computation ─────────────────────────────────────────
    getViewMatrix() {
        if (this._viewDirty) {
            mat4.lookAt(this._viewMatrix, this._position, this._target, this._up);
            this._viewDirty = false;
            this._vpDirty = true;
        }
        return this._viewMatrix;
    }
    getProjectionMatrix() {
        if (this._projDirty) {
            if (this._mode === 'perspective') {
                // Depth precision is spent ∝ 1/near (hyperbolic 1/z buffer) — with autoNear the near plane tracks
                // the orbit distance (2% of it, clamped), giving ~40× finer far-field depth at typical framing
                // without clipping close-ups. Ortho ignores this (linear depth). docs/specs/depth-precision.md.
                // autoFar (if on) tracks the scene so a big world never clips at grazing angles / when dollying.
                const far = this.effectiveFar;
                const near = this._autoNear
                    ? Math.min(Math.max(vec3.distance(this._position, this._target) * 0.02, 0.02), Math.min(0.5, far * 0.5))
                    : this._near;
                // perspectiveZO maps depth to [0,1] (WebGPU NDC convention).
                // mat4.perspective maps to [-1,1] (OpenGL) which also works for perspective
                // because the depth warp bunches values near z_ndc=1, but ZO is more correct.
                mat4.perspectiveZO?.(this._projMatrix, this._fov, this._aspect, near, far)
                    ?? mat4.perspective(this._projMatrix, this._fov, this._aspect, near, far);
            }
            else {
                const hh = this._orthoSize;
                const hw = hh * this._aspect;
                const ox = this._orthoOffsetX;
                const oy = this._orthoOffsetY;
                // orthoZO maps depth to [0,1] (WebGPU NDC convention).
                // mat4.ortho maps to [-1,1]: a mesh 100 units in front gives z_ndc≈-0.8 → clipped by WebGPU.
                // ox/oy shift the frustum in camera space without moving target, enabling armature pan.
                mat4.orthoZO?.(this._projMatrix, -hw + ox, hw + ox, -hh + oy, hh + oy, this._near, this._far)
                    ?? mat4.ortho(this._projMatrix, -hw + ox, hw + ox, -hh + oy, hh + oy, this._near, this._far);
            }
            this._projDirty = false;
            this._vpDirty = true;
        }
        return this._projMatrix;
    }
    getViewProjectionMatrix() {
        // Force recompute of view/projection if dirty
        this.getViewMatrix();
        this.getProjectionMatrix();
        if (this._vpDirty) {
            mat4.mul(this._vpMatrix, this._projMatrix, this._viewMatrix);
            this._vpDirty = false;
        }
        return this._vpMatrix;
    }
    // ── Dirty flags ────────────────────────────────────────────────
    markViewDirty() {
        this._viewDirty = true;
        this._vpDirty = true;
        // autoNear/autoFar derive the near/far plane from camera→target distance — a moved camera changes the projection.
        if (this._autoNear || this._autoFar)
            this._projDirty = true;
    }
    markProjDirty() {
        this._projDirty = true;
        this._vpDirty = true;
    }
    // ── Serialization ──────────────────────────────────────────────
    toJSON() {
        return {
            position: [this._position[0], this._position[1], this._position[2]],
            target: [this._target[0], this._target[1], this._target[2]],
            up: [this._up[0], this._up[1], this._up[2]],
            fov: this._fov,
            near: this._near,
            far: this._far,
            orthoSize: this._orthoSize,
            mode: this._mode,
        };
    }
}
