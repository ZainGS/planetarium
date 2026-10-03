/** api.ts — types for `window.planetarium`, the bridge defined in web/bridge.js. */
/**
 * Display names for agents. Subagents are named by type ("general-purpose"); when several
 * live ones share a type, each gets a short id so you can tell them apart ("general-purpose · 3f9a").
 */
export function agentDisplayNames(list) {
    const base = (a) => (a.parentKey ? a.agentType : a.title || `Claude · ${a.sessionId.slice(0, 4)}`);
    const counts = new Map();
    for (const a of list)
        if (a.status !== 'done')
            counts.set(base(a), (counts.get(base(a)) ?? 0) + 1);
    const out = new Map();
    for (const a of list) {
        const b = base(a);
        out.set(a.key, a.parentKey && (counts.get(b) ?? 0) > 1 && a.agentId ? `${b} · ${a.agentId.slice(0, 4)}` : b);
    }
    return out;
}
/** A talk-mode collision that's still being worked out (or waiting for you). */
export function isOpenTalk(c) {
    return c.mode === 'talk' && (c.status === 'talking' || c.status === 'asked_to_wait' || c.status === 'needs_you');
}
export function getBridge() {
    if (!window.planetarium)
        throw new Error('Planetarium bridge missing: this page must run inside the Planetarium app.');
    return window.planetarium;
}
