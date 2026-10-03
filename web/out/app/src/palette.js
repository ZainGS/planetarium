/** palette.ts — one color per repo. Agents working in a repo will glow in that repo's color. */
export const REPO_COLORS = [
    '#7cc4ff', // sky
    '#ffa36b', // amber
    '#b49cff', // violet
    '#6ee7b7', // mint
    '#ff7aa8', // rose
    '#ffd166', // gold
    '#4fd1c5', // teal
    '#b5e853', // lime
    '#ff6b6b', // coral
    '#8fa8ff', // periwinkle
];
export function hexToRgb(hex) {
    const n = parseInt(hex.slice(1), 16);
    return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}
export function repoColorHex(colorIndex) {
    return REPO_COLORS[((colorIndex % REPO_COLORS.length) + REPO_COLORS.length) % REPO_COLORS.length];
}
export function repoColor(colorIndex) {
    return hexToRgb(repoColorHex(colorIndex));
}
/** Blend toward white (t > 0) or toward grey (t < 0, desaturate). */
export function tint(c, t) {
    if (t >= 0)
        return [c[0] + (1 - c[0]) * t, c[1] + (1 - c[1]) * t, c[2] + (1 - c[2]) * t];
    const g = (c[0] + c[1] + c[2]) / 3;
    const k = -t;
    return [c[0] + (g - c[0]) * k, c[1] + (g - c[1]) * k, c[2] + (g - c[2]) * k];
}
