/**
 * math.ts — the tiny slice of gl-matrix that the camera and orbit controller use.
 *
 * Function names and argument order match gl-matrix (`out` first), so the files copied from
 * Salsa only needed their import path changed. Column-major Float32Array matrices, the same
 * memory layout WGSL expects for `mat4x4f`.
 */

export type vec3 = Float32Array;
export type mat4 = Float32Array;

export const vec3 = {
  create(): vec3 { return new Float32Array(3); },
  fromValues(x: number, y: number, z: number): vec3 { const o = new Float32Array(3); o[0] = x; o[1] = y; o[2] = z; return o; },
  set(o: vec3, x: number, y: number, z: number): vec3 { o[0] = x; o[1] = y; o[2] = z; return o; },
  copy(o: vec3, a: ArrayLike<number>): vec3 { o[0] = a[0]; o[1] = a[1]; o[2] = a[2]; return o; },
  add(o: vec3, a: ArrayLike<number>, b: ArrayLike<number>): vec3 { o[0] = a[0] + b[0]; o[1] = a[1] + b[1]; o[2] = a[2] + b[2]; return o; },
  sub(o: vec3, a: ArrayLike<number>, b: ArrayLike<number>): vec3 { o[0] = a[0] - b[0]; o[1] = a[1] - b[1]; o[2] = a[2] - b[2]; return o; },
  scaleAndAdd(o: vec3, a: ArrayLike<number>, b: ArrayLike<number>, s: number): vec3 {
    o[0] = a[0] + b[0] * s; o[1] = a[1] + b[1] * s; o[2] = a[2] + b[2] * s; return o;
  },
  cross(o: vec3, a: ArrayLike<number>, b: ArrayLike<number>): vec3 {
    const ax = a[0], ay = a[1], az = a[2], bx = b[0], by = b[1], bz = b[2];
    o[0] = ay * bz - az * by; o[1] = az * bx - ax * bz; o[2] = ax * by - ay * bx; return o;
  },
  normalize(o: vec3, a: ArrayLike<number>): vec3 {
    const l = Math.hypot(a[0], a[1], a[2]);
    const s = l > 0 ? 1 / l : 0;
    o[0] = a[0] * s; o[1] = a[1] * s; o[2] = a[2] * s; return o;
  },
  distance(a: ArrayLike<number>, b: ArrayLike<number>): number { return Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2]); },
};

export const mat4 = {
  create(): mat4 { const o = new Float32Array(16); o[0] = o[5] = o[10] = o[15] = 1; return o; },

  lookAt(o: mat4, eye: ArrayLike<number>, center: ArrayLike<number>, up: ArrayLike<number>): mat4 {
    let z0 = eye[0] - center[0], z1 = eye[1] - center[1], z2 = eye[2] - center[2];
    let len = Math.hypot(z0, z1, z2);
    if (len < 1e-9) { o.fill(0); o[0] = o[5] = o[10] = o[15] = 1; return o; }
    z0 /= len; z1 /= len; z2 /= len;
    let x0 = up[1] * z2 - up[2] * z1, x1 = up[2] * z0 - up[0] * z2, x2 = up[0] * z1 - up[1] * z0;
    len = Math.hypot(x0, x1, x2);
    if (len > 0) { x0 /= len; x1 /= len; x2 /= len; } else { x0 = x1 = x2 = 0; }
    const y0 = z1 * x2 - z2 * x1, y1 = z2 * x0 - z0 * x2, y2 = z0 * x1 - z1 * x0;
    o[0] = x0; o[1] = y0; o[2] = z0; o[3] = 0;
    o[4] = x1; o[5] = y1; o[6] = z1; o[7] = 0;
    o[8] = x2; o[9] = y2; o[10] = z2; o[11] = 0;
    o[12] = -(x0 * eye[0] + x1 * eye[1] + x2 * eye[2]);
    o[13] = -(y0 * eye[0] + y1 * eye[1] + y2 * eye[2]);
    o[14] = -(z0 * eye[0] + z1 * eye[1] + z2 * eye[2]);
    o[15] = 1;
    return o;
  },

  /** Perspective projection with depth mapped to [0, 1] (WebGPU convention). */
  perspectiveZO(o: mat4, fovy: number, aspect: number, near: number, far: number): mat4 {
    const f = 1 / Math.tan(fovy / 2);
    o.fill(0);
    o[0] = f / aspect; o[5] = f; o[11] = -1;
    const nf = 1 / (near - far);
    o[10] = far * nf; o[14] = far * near * nf;
    return o;
  },
  perspective(o: mat4, fovy: number, aspect: number, near: number, far: number): mat4 {
    return mat4.perspectiveZO(o, fovy, aspect, near, far);
  },

  /** Orthographic projection with depth mapped to [0, 1]. */
  orthoZO(o: mat4, l: number, r: number, b: number, t: number, near: number, far: number): mat4 {
    const lr = 1 / (l - r), bt = 1 / (b - t), nf = 1 / (near - far);
    o.fill(0);
    o[0] = -2 * lr; o[5] = -2 * bt; o[10] = nf; o[15] = 1;
    o[12] = (l + r) * lr; o[13] = (t + b) * bt; o[14] = near * nf;
    return o;
  },
  ortho(o: mat4, l: number, r: number, b: number, t: number, near: number, far: number): mat4 {
    return mat4.orthoZO(o, l, r, b, t, near, far);
  },

  mul(o: mat4, a: ArrayLike<number>, b: ArrayLike<number>): mat4 {
    const r = new Float32Array(16);
    for (let c = 0; c < 4; c++) {
      for (let row = 0; row < 4; row++) {
        r[c * 4 + row] = a[row] * b[c * 4] + a[4 + row] * b[c * 4 + 1] + a[8 + row] * b[c * 4 + 2] + a[12 + row] * b[c * 4 + 3];
      }
    }
    o.set(r);
    return o;
  },
};

/** Transform a world-space point by a column-major 4x4 matrix into clip space. */
export function transformPoint(m: ArrayLike<number>, x: number, y: number, z: number, out: Float32Array): Float32Array {
  out[0] = m[0] * x + m[4] * y + m[8] * z + m[12];
  out[1] = m[1] * x + m[5] * y + m[9] * z + m[13];
  out[2] = m[2] * x + m[6] * y + m[10] * z + m[14];
  out[3] = m[3] * x + m[7] * y + m[11] * z + m[15];
  return out;
}
