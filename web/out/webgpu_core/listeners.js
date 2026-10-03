/**
 * listeners.ts — stand-in for Salsa's `zoneless-listeners.ts`.
 *
 * Salsa needs that helper to keep Angular's Zone.js from running change detection on every
 * pointer move. Planetarium has no Angular, so these are plain add/removeEventListener calls.
 * The names are kept so `orbit-controller.ts` stays identical to Salsa's apart from imports.
 */
export function addZonelessListener(el, type, handler, options) {
    el.addEventListener(type, handler, options);
}
export function removeZonelessListener(el, type, handler, options) {
    el.removeEventListener(type, handler, options);
}
