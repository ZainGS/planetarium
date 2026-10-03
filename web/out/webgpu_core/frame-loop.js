/**
 * frame-loop.ts — on-demand rendering.
 *
 * Nothing draws unless something asked for a frame. Animations (camera damping, fly-to tweens,
 * later: moving agents) register as "animators" that return true while they still need frames;
 * once every animator is idle the loop stops and the GPU goes quiet. A constellation you leave
 * open next to your agents should cost close to nothing while it's not changing.
 */
export class FrameLoop {
    renderFrame;
    rafId = null;
    lastTime = 0;
    animators = new Set();
    frameRequested = false;
    paused = false;
    constructor(renderFrame) {
        this.renderFrame = renderFrame;
    }
    /** Draw one more frame soon. Safe to call any number of times per frame. */
    requestRender() {
        this.frameRequested = true;
        this.schedule();
    }
    /** Keep rendering while `fn` returns true. It is dropped automatically once it returns false. */
    addAnimator(fn) {
        this.animators.add(fn);
        this.requestRender();
    }
    removeAnimator(fn) { this.animators.delete(fn); }
    get isAnimating() { return this.animators.size > 0; }
    /**
     * Stop drawing entirely (e.g. while the window is hidden in the tray). Animators are kept and
     * pick up where the clock is when drawing resumes.
     */
    setPaused(paused) {
        if (paused === this.paused)
            return;
        this.paused = paused;
        if (paused) {
            if (this.rafId !== null)
                cancelAnimationFrame(this.rafId);
            this.rafId = null;
            this.lastTime = 0;
        }
        else {
            this.requestRender();
        }
    }
    schedule() {
        if (this.paused || this.rafId !== null)
            return;
        this.rafId = requestAnimationFrame((t) => this.tick(t));
    }
    tick(time) {
        this.rafId = null;
        const dt = this.lastTime ? Math.min(time - this.lastTime, 100) : 16;
        this.lastTime = time;
        for (const fn of [...this.animators]) {
            if (!fn(time, dt))
                this.animators.delete(fn);
        }
        this.frameRequested = false;
        this.renderFrame(time);
        if (this.animators.size > 0 || this.frameRequested)
            this.schedule();
        else
            this.lastTime = 0;
    }
}
