/**
 * webgpu_core — Planetarium's small WebGPU engine.
 *
 * Knows about cameras, glowing points, lines and picking. Knows nothing about repos or agents;
 * keep it that way so it can move into Salsa later as a reusable module.
 */
export { Camera3D } from './camera-3d.js';
export { OrbitController } from './orbit-controller.js';
export { createGpuContext, syncCanvasSize, WebGPUUnavailableError } from './device.js';
export { StarPass, STAR_FLOATS } from './star-pass.js';
export { LinePass, LINE_VERTEX_FLOATS } from './line-pass.js';
export { pickNearest, projectToScreen } from './picker.js';
export { FrameLoop } from './frame-loop.js';
export { mat4, vec3, transformPoint } from './math.js';
