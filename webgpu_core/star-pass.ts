/**
 * star-pass.ts — instanced, camera-facing glowing points.
 *
 * One draw call for any number of stars. Each star is a quad expanded in the vertex shader
 * (no vertex buffer for the corners), sized in world units but clamped to a minimum on-screen
 * size so distant stars never vanish. Blending is additive, so draw order doesn't matter and
 * no depth buffer is needed.
 *
 * Instance layout (12 floats = 48 bytes):
 *   [0..2] position xyz   [3] glow radius (world units)
 *   [4..7] color rgba     [8] highlight (0 = none, 1 = hover, 2 = selected)
 *   [9] min radius (px)   [10] core size (0..1 of the radius)   [11] unused
 */

export const STAR_FLOATS = 12;

const SHADER = /* wgsl */ `
struct Uniforms {
  viewProj : mat4x4f,
  // x,y = viewport size in px, z = projection scale (P[1][1] * height / 2), w = global brightness
  viewport : vec4f,
};
@group(0) @binding(0) var<uniform> u : Uniforms;

struct Inst {
  @location(0) pos    : vec3f,
  @location(1) size   : f32,
  @location(2) color  : vec4f,
  @location(3) params : vec4f,
};

struct VOut {
  @builtin(position) clip : vec4f,
  @location(0) uv     : vec2f,
  @location(1) color  : vec4f,
  @location(2) params : vec4f,
};

@vertex
fn vs(@builtin(vertex_index) vi : u32, inst : Inst) -> VOut {
  var corners = array<vec2f, 6>(
    vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0),
    vec2f(-1.0,  1.0), vec2f(1.0, -1.0), vec2f(1.0,  1.0));
  let c = corners[vi];
  var out : VOut;
  var clip = u.viewProj * vec4f(inst.pos, 1.0);
  if (clip.w <= 0.0001) {
    out.clip = vec4f(2.0, 2.0, 2.0, 1.0);   // behind the camera: push off-screen
    return out;
  }
  let hl = inst.params.x;
  let grow = select(1.0, select(1.35, 1.6, hl > 1.5), hl > 0.5);
  let radiusPx = clamp(inst.size * grow * u.viewport.z / clip.w, inst.params.y * grow, 260.0);
  let offset = c * radiusPx * 2.0 / u.viewport.xy;
  out.clip = vec4f(clip.xy + offset * clip.w, clip.z, clip.w);
  out.uv = c;
  out.color = inst.color;
  out.params = vec4f(hl, radiusPx, inst.params.z, 0.0);
  return out;
}

@fragment
fn fs(v : VOut) -> @location(0) vec4f {
  let r = length(v.uv);
  if (r > 1.0) { discard; }
  let coreSize = max(v.params.z, 0.05);
  let core = smoothstep(coreSize, 0.0, r);
  let glow = exp(-r * r * 7.0) * (1.0 - smoothstep(0.85, 1.0, r));
  var a = (core + glow * 0.55) * v.color.a;
  var rgb = mix(v.color.rgb, vec3f(1.0), core * 0.65);
  // Hover / selection ring, a couple of pixels wide whatever the star's size.
  let hl = v.params.x;
  if (hl > 0.5) {
    let ringW = 2.0 / max(v.params.y, 1.0);
    let ring = smoothstep(ringW, 0.0, abs(r - 0.78)) * select(0.55, 0.95, hl > 1.5);
    a = a + ring;
    rgb = mix(rgb, vec3f(1.0), ring / max(a, 0.001));
  }
  a = a * u.viewport.w;
  return vec4f(rgb * a, a);
}
`;

export class StarPass {
  private readonly device: GPUDevice;
  private readonly pipeline: GPURenderPipeline;
  private readonly uniformBuffer: GPUBuffer;
  private readonly bindGroup: GPUBindGroup;
  private instanceBuffer: GPUBuffer | null = null;
  private capacity = 0;
  private count = 0;
  private readonly uniformData = new Float32Array(20);

  constructor(device: GPUDevice, format: GPUTextureFormat, label = 'stars') {
    this.device = device;
    const module = device.createShaderModule({ code: SHADER, label });
    const additive: GPUBlendState = {
      color: { srcFactor: 'one', dstFactor: 'one', operation: 'add' },
      alpha: { srcFactor: 'one', dstFactor: 'one', operation: 'add' },
    };
    this.pipeline = device.createRenderPipeline({
      label,
      layout: 'auto',
      vertex: {
        module,
        entryPoint: 'vs',
        buffers: [{
          arrayStride: STAR_FLOATS * 4,
          stepMode: 'instance',
          attributes: [
            { shaderLocation: 0, offset: 0, format: 'float32x3' },
            { shaderLocation: 1, offset: 12, format: 'float32' },
            { shaderLocation: 2, offset: 16, format: 'float32x4' },
            { shaderLocation: 3, offset: 32, format: 'float32x4' },
          ],
        }],
      },
      fragment: { module, entryPoint: 'fs', targets: [{ format, blend: additive }] },
      primitive: { topology: 'triangle-list' },
    });
    this.uniformBuffer = device.createBuffer({ size: 80, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST, label: `${label}-uniforms` });
    this.bindGroup = device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [{ binding: 0, resource: { buffer: this.uniformBuffer } }],
    });
  }

  get instanceCount(): number { return this.count; }

  /** Replace all instances. `data` holds `count * STAR_FLOATS` floats. */
  setInstances(data: Float32Array, count: number): void {
    this.count = count;
    if (count === 0) return;
    const bytes = count * STAR_FLOATS * 4;
    if (!this.instanceBuffer || this.capacity < count) {
      this.instanceBuffer?.destroy();
      this.capacity = Math.max(count, Math.ceil(this.capacity * 1.5), 64);
      this.instanceBuffer = this.device.createBuffer({
        size: this.capacity * STAR_FLOATS * 4,
        usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
        label: 'star-instances',
      });
    }
    this.device.queue.writeBuffer(this.instanceBuffer, 0, data.buffer, data.byteOffset, bytes);
  }

  /** Update a single instance in place (hover / selection changes). */
  updateInstance(index: number, data: Float32Array): void {
    if (!this.instanceBuffer || index < 0 || index >= this.count) return;
    this.device.queue.writeBuffer(this.instanceBuffer, index * STAR_FLOATS * 4, data.buffer, data.byteOffset + index * STAR_FLOATS * 4, STAR_FLOATS * 4);
  }

  draw(pass: GPURenderPassEncoder, viewProj: ArrayLike<number>, width: number, height: number, projScale: number, brightness = 1): void {
    if (this.count === 0 || !this.instanceBuffer) return;
    const u = this.uniformData;
    for (let i = 0; i < 16; i++) u[i] = viewProj[i];
    u[16] = width; u[17] = height; u[18] = projScale; u[19] = brightness;
    this.device.queue.writeBuffer(this.uniformBuffer, 0, u);
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.setVertexBuffer(0, this.instanceBuffer);
    pass.draw(6, this.count);
  }

  destroy(): void {
    this.instanceBuffer?.destroy();
    this.uniformBuffer.destroy();
  }
}
