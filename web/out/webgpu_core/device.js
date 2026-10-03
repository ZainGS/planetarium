/**
 * device.ts — WebGPU adapter/device/canvas setup and DPI-aware resizing.
 *
 * The canvas is configured with premultiplied alpha and cleared to transparent, so whatever
 * the page draws behind it (a CSS gradient) shows through and the additive star glow sits on top.
 */
export class WebGPUUnavailableError extends Error {
}
export async function createGpuContext(canvas) {
    if (!('gpu' in navigator) || !navigator.gpu) {
        throw new WebGPUUnavailableError('This window has no WebGPU support (navigator.gpu is missing).');
    }
    const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'low-power' })
        ?? await navigator.gpu.requestAdapter();
    if (!adapter)
        throw new WebGPUUnavailableError('No WebGPU adapter was found for this GPU.');
    const device = await adapter.requestDevice();
    const context = canvas.getContext('webgpu');
    if (!context)
        throw new WebGPUUnavailableError('Could not create a WebGPU canvas context.');
    const format = navigator.gpu.getPreferredCanvasFormat();
    context.configure({ device, format, alphaMode: 'premultiplied' });
    const gpu = { device, context, format, canvas, width: 0, height: 0, dpr: 1 };
    syncCanvasSize(gpu);
    return gpu;
}
/** Match the canvas backing store to its CSS size. Returns true if the size changed. */
export function syncCanvasSize(gpu) {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const rect = gpu.canvas.getBoundingClientRect();
    const w = Math.max(1, Math.round(rect.width * dpr));
    const h = Math.max(1, Math.round(rect.height * dpr));
    if (w === gpu.width && h === gpu.height && dpr === gpu.dpr)
        return false;
    gpu.canvas.width = w;
    gpu.canvas.height = h;
    gpu.width = w;
    gpu.height = h;
    gpu.dpr = dpr;
    return true;
}
