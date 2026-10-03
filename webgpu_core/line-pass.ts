/**
 * line-pass.ts — thin additive lines (folder links, orbit rings, later: agent trails).
 *
 * Plain `line-list` topology: two vertices per segment, each with its own color so a line can
 * fade along its length. Lines are 1 physical pixel wide, which reads as delicate threads at
 * typical constellation zoom levels.
 *
 * Vertex layout (7 floats = 28 bytes): position xyz, color rgba (straight alpha).
 */

export const LINE_VERTEX_FLOATS = 7;

const SHADER = /* wgsl */ `
struct Uniforms { viewProj : mat4x4f, params : vec4f };   // params.x = global brightness
@group(0) @binding(0) var<uniform> u : Uniforms;

struct VOut {
  @builtin(position) clip : vec4f,
  @location(0) color : vec4f,
};

@vertex
fn vs(@location(0) pos : vec3f, @location(1) color : vec4f) -> VOut {
  var out : VOut;
  out.clip = u.viewProj * vec4f(pos, 1.0);
  out.color = color;
  return out;
}

@fragment
fn fs(v : VOut) -> @location(0) vec4f {
  let a = v.color.a * u.params.x;
  return vec4f(v.color.rgb * a, a);
}
`;

export class LinePass {
  private readonly device: GPUDevice;
  private readonly pipeline: GPURenderPipeline;
  private readonly uniformBuffer: GPUBuffer;
  private readonly bindGroup: GPUBindGroup;
  private vertexBuffer: GPUBuffer | null = null;
  private capacity = 0;
  private vertexCount = 0;
  private readonly uniformData = new Float32Array(20);

  constructor(device: GPUDevice, format: GPUTextureFormat, label = 'lines') {
    this.device = device;
    const module = device.createShaderModule({ code: SHADER, label });
    this.pipeline = device.createRenderPipeline({
      label,
      layout: 'auto',
      vertex: {
        module,
        entryPoint: 'vs',
        buffers: [{
          arrayStride: LINE_VERTEX_FLOATS * 4,
          attributes: [
            { shaderLocation: 0, offset: 0, format: 'float32x3' },
            { shaderLocation: 1, offset: 12, format: 'float32x4' },
          ],
        }],
      },
      fragment: {
        module,
        entryPoint: 'fs',
        targets: [{
          format,
          blend: {
            color: { srcFactor: 'one', dstFactor: 'one', operation: 'add' },
            alpha: { srcFactor: 'one', dstFactor: 'one', operation: 'add' },
          },
        }],
      },
      primitive: { topology: 'line-list' },
    });
    this.uniformBuffer = device.createBuffer({ size: 80, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST, label: `${label}-uniforms` });
    this.bindGroup = device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [{ binding: 0, resource: { buffer: this.uniformBuffer } }],
    });
  }

  /** Replace all vertices. `data` holds `vertexCount * LINE_VERTEX_FLOATS` floats (two per segment). */
  setVertices(data: Float32Array, vertexCount: number): void {
    this.vertexCount = vertexCount;
    if (vertexCount === 0) return;
    if (!this.vertexBuffer || this.capacity < vertexCount) {
      this.vertexBuffer?.destroy();
      this.capacity = Math.max(vertexCount, Math.ceil(this.capacity * 1.5), 128);
      this.vertexBuffer = this.device.createBuffer({
        size: this.capacity * LINE_VERTEX_FLOATS * 4,
        usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
        label: 'line-vertices',
      });
    }
    this.device.queue.writeBuffer(this.vertexBuffer, 0, data.buffer, data.byteOffset, vertexCount * LINE_VERTEX_FLOATS * 4);
  }

  draw(pass: GPURenderPassEncoder, viewProj: ArrayLike<number>, brightness = 1): void {
    if (this.vertexCount === 0 || !this.vertexBuffer) return;
    const u = this.uniformData;
    for (let i = 0; i < 16; i++) u[i] = viewProj[i];
    u[16] = brightness;
    this.device.queue.writeBuffer(this.uniformBuffer, 0, u);
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.setVertexBuffer(0, this.vertexBuffer);
    pass.draw(this.vertexCount);
  }

  destroy(): void {
    this.vertexBuffer?.destroy();
    this.uniformBuffer.destroy();
  }
}
