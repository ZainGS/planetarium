/**
 * main.ts — Planetarium's page: repo list, add/remove, and wiring the constellation view.
 */
import { getBridge, isOpenTalk, agentDisplayNames } from './api.js';
import { buildTree } from './tree.js';
import { layoutRepo, placeClusters } from './layout.js';
import { buildScene } from './scene.js';
import { repoColorHex } from './palette.js';
import { ConstellationView } from './view.js';
import { WebGPUUnavailableError } from '../../webgpu_core/index.js';
const $ = (id) => document.getElementById(id);
const els = {
    canvas: $('sky'),
    labels: $('labels'),
    tooltip: $('tooltip'),
    details: $('details'),
    empty: $('empty'),
    emptyBrowse: $('empty-browse'),
    gpuError: $('gpu-error'),
    dropHint: $('drop-hint'),
    pathInput: $('path-input'),
    addForm: $('add-form'),
    browseBtn: $('browse-btn'),
    addMsg: $('add-msg'),
    repoList: $('repo-list'),
    repoCount: $('repo-count'),
    frameBtn: $('frame-btn'),
    agentList: $('agent-list'),
    agentCount: $('agent-count'),
    claudeConnect: $('claude-connect'),
    usage: $('usage'),
    windDown: $('wind-down'),
    collisionList: $('collision-list'),
    collisionCard: $('collision-card'),
    settings: $('settings'),
    settingsBtn: $('settings-btn'),
};
const repos = new Map();
/** Where each constellation sat last time, so rescans don't shuffle them around. */
const lastCenters = new Map();
let view = null;
let bridge;
let firstFrameDone = false;
let agents = [];
let claude = null;
/** Which agent the details panel is showing, so it can refresh as the agent moves. */
let detailsAgentKey = null;
let collisions = [];
let coordSettings = null;
/** The Ask-mode collision whose card is showing. */
let cardCollisionId = null;
let cardTimer = 0;
/** Collisions you've answered (or set aside), so the card doesn't reopen before the update arrives. */
const answered = new Set();
const setAside = new Set();
function orderedRepos() {
    return [...repos.values()].sort((a, b) => a.meta.addedAt - b.meta.addedAt);
}
// ── Messages ──────────────────────────────────────────────────────────
let msgTimer = 0;
function showMessage(text, kind = 'info') {
    els.addMsg.textContent = text;
    els.addMsg.dataset.kind = kind;
    els.addMsg.hidden = !text;
    clearTimeout(msgTimer);
    if (text)
        msgTimer = window.setTimeout(() => { els.addMsg.hidden = true; }, kind === 'error' ? 9000 : 5000);
}
// ── Scene rebuild ─────────────────────────────────────────────────────
function rebuildScene() {
    if (!view)
        return;
    const ready = orderedRepos().filter((r) => r.status === 'ready' && r.layout);
    const centers = placeClusters(ready.map((r) => r.layout.radius), ready.map((r) => r.meta.id), lastCenters);
    lastCenters.clear();
    ready.forEach((r, i) => lastCenters.set(r.meta.id, centers[i]));
    const sceneRepos = ready.map((r, i) => ({
        id: r.meta.id, name: r.meta.name, colorIndex: r.meta.colorIndex, layout: r.layout, center: centers[i],
    }));
    view.setScene(buildScene(sceneRepos));
    view.setAgents(agents.filter((a) => a.repoId && repos.has(a.repoId)));
    renderAgentList();
    els.empty.hidden = repos.size > 0;
    els.frameBtn.hidden = repos.size === 0;
}
function applyScan(state, scan) {
    state.scan = scan;
    if (!scan.ok) {
        state.status = 'error';
        state.error = scan.error ?? 'Scan failed';
        state.tree = null;
        state.layout = null;
        return;
    }
    state.tree = buildTree(scan.files, state.meta.name);
    state.layout = layoutRepo(state.tree);
    state.status = 'ready';
    state.error = null;
}
async function scan(state) {
    state.status = 'scanning';
    renderRepoList();
    const result = await bridge.scanRepo(state.meta.id);
    if (!repos.has(state.meta.id))
        return; // removed while scanning
    applyScan(state, result);
    renderRepoList();
    rebuildScene();
}
// ── Repo list ─────────────────────────────────────────────────────────
function sceneIndexOf(id) {
    return view?.getScene()?.repos.findIndex((r) => r.id === id) ?? -1;
}
function renderRepoList() {
    const list = orderedRepos();
    els.repoCount.textContent = list.length ? String(list.length) : '';
    els.repoList.replaceChildren(...list.map((r) => {
        const li = document.createElement('li');
        li.className = `repo status-${r.status}`;
        li.style.setProperty('--repo-color', repoColorHex(r.meta.colorIndex));
        li.title = r.meta.path;
        const main = document.createElement('button');
        main.className = 'repo-main';
        main.type = 'button';
        main.addEventListener('click', () => {
            const idx = sceneIndexOf(r.meta.id);
            if (idx >= 0)
                view?.flyToRepo(idx);
        });
        const dot = document.createElement('span');
        dot.className = 'dot';
        const text = document.createElement('span');
        text.className = 'repo-text';
        const name = document.createElement('span');
        name.className = 'repo-name';
        name.textContent = r.meta.name;
        const path = document.createElement('span');
        path.className = 'repo-path';
        path.textContent = `\u200E${r.meta.path}\u200E`; // keep the path left-to-right inside the RTL ellipsis box
        const stats = document.createElement('span');
        stats.className = 'repo-stats';
        if (r.status === 'scanning')
            stats.textContent = 'Scanning…';
        else if (r.status === 'error')
            stats.textContent = r.error ?? 'Could not scan';
        else if (r.layout && r.scan) {
            const parts = [`${r.layout.fileCount.toLocaleString()} files`, `${r.layout.folderCount.toLocaleString()} folders`];
            parts.push(r.scan.method === 'git' ? 'git' : r.scan.gitRoots.length ? `${r.scan.gitRoots.length} git repo${r.scan.gitRoots.length > 1 ? 's' : ''} inside` : 'folder scan');
            if (r.scan.truncated)
                parts.push('truncated');
            stats.textContent = parts.join(' · ');
        }
        text.append(name, path, stats);
        main.append(dot, text);
        const actions = document.createElement('span');
        actions.className = 'repo-actions';
        const rescan = iconButton('Rescan', '↻', () => void scan(r));
        const remove = iconButton('Remove from Planetarium', '×', () => {
            if (remove.dataset.confirm !== '1') {
                remove.dataset.confirm = '1';
                remove.textContent = 'Remove?';
                remove.classList.add('confirm');
                window.setTimeout(() => {
                    if (remove.isConnected) {
                        remove.dataset.confirm = '';
                        remove.textContent = '×';
                        remove.classList.remove('confirm');
                    }
                }, 3000);
                return;
            }
            void removeRepo(r.meta.id);
        });
        actions.append(rescan, remove);
        li.append(main, actions);
        return li;
    }));
}
function iconButton(label, glyph, onClick) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'icon-btn';
    b.title = label;
    b.setAttribute('aria-label', label);
    b.textContent = glyph;
    b.addEventListener('click', (e) => { e.stopPropagation(); onClick(); });
    return b;
}
// ── Add / remove ──────────────────────────────────────────────────────
async function addRepo(folder) {
    const res = await bridge.addRepo(folder ?? null);
    if (res.canceled)
        return;
    if (!res.ok || !res.repo) {
        showMessage(res.error ?? 'Could not add that folder.', 'error');
        if (res.code === 'DUPLICATE' && res.repoId) {
            const idx = sceneIndexOf(res.repoId);
            if (idx >= 0)
                view?.flyToRepo(idx);
        }
        return;
    }
    els.pathInput.value = '';
    showMessage(`Added ${res.repo.name}.`);
    const state = { meta: res.repo, status: 'scanning', scan: null, tree: null, layout: null, error: null };
    repos.set(res.repo.id, state);
    els.empty.hidden = true;
    els.frameBtn.hidden = false;
    await scan(state);
    const idx = sceneIndexOf(res.repo.id);
    if (idx >= 0) {
        if ((view?.getScene()?.repos.length ?? 0) > 1)
            view?.frameAll(true);
        else
            view?.flyToRepo(idx);
    }
}
async function removeRepo(id) {
    await bridge.removeRepo(id);
    const name = repos.get(id)?.meta.name;
    repos.delete(id);
    hideDetails();
    renderRepoList();
    rebuildScene();
    if (name)
        showMessage(`Removed ${name}.`);
}
function starInfo(star) {
    const s = view?.getScene();
    if (!s || star < 0 || star >= s.starCount)
        return null;
    const repo = s.repos[s.starRepo[star]];
    const ni = s.starNode[star];
    return { repo, node: repo.layout.nodes[ni].node, isRoot: ni === 0 };
}
function describe(info) {
    if (info.isRoot)
        return `Repository · ${info.node.fileCount.toLocaleString()} files`;
    if (info.node.isDir)
        return `Folder · ${info.node.fileCount.toLocaleString()} files${info.node.folderCount ? ` · ${info.node.folderCount.toLocaleString()} subfolders` : ''}`;
    return 'File';
}
function showTooltip(star, x, y) {
    const info = starInfo(star);
    if (!info) {
        els.tooltip.hidden = true;
        return;
    }
    els.tooltip.style.setProperty('--repo-color', repoColorHex(info.repo.colorIndex));
    els.tooltip.replaceChildren(textEl('strong', info.isRoot ? info.repo.name : info.node.name), textEl('span', info.isRoot ? describe(info) : `${info.repo.name}/${info.node.path}`));
    els.tooltip.hidden = false;
    const stage = els.canvas.getBoundingClientRect();
    const tw = els.tooltip.offsetWidth;
    const left = Math.min(x + 16, stage.width - tw - 12);
    els.tooltip.style.transform = `translate(${Math.max(8, left)}px, ${y + 18}px)`;
}
function textEl(tag, text, cls) {
    const e = document.createElement(tag);
    e.textContent = text;
    if (cls)
        e.className = cls;
    return e;
}
function hideDetails() {
    els.details.hidden = true;
    detailsAgentKey = null;
    view?.selectAgent(null);
    view?.setSelected(-1);
}
function showDetails(star) {
    detailsAgentKey = null;
    const info = starInfo(star);
    if (!info) {
        els.details.hidden = true;
        return;
    }
    const color = repoColorHex(info.repo.colorIndex);
    els.details.style.setProperty('--repo-color', color);
    const header = document.createElement('header');
    const kind = textEl('span', info.isRoot ? 'Repository' : info.node.isDir ? 'Folder' : 'File', 'kind');
    const close = iconButton('Close', '×', hideDetails);
    header.append(kind, close);
    const title = textEl('h3', info.isRoot ? info.repo.name : info.node.name);
    const path = textEl('p', info.isRoot ? (repos.get(info.repo.id)?.meta.path ?? '') : `${info.repo.name}/${info.node.path}`, 'path');
    const facts = document.createElement('dl');
    const addFact = (k, v) => { facts.append(textEl('dt', k), textEl('dd', v)); };
    if (info.node.isDir) {
        addFact('Files', info.node.fileCount.toLocaleString());
        addFact('Subfolders', info.node.folderCount.toLocaleString());
    }
    if (!info.isRoot)
        addFact('Repository', info.repo.name);
    const here = agentsAt(info.repo.id, info.isRoot ? '' : info.node.path, info.node.isDir);
    addFact('Agents here', here.length ? here.map(agentName).join(', ') : 'None');
    const actions = document.createElement('div');
    actions.className = 'details-actions';
    const fly = document.createElement('button');
    fly.type = 'button';
    fly.className = 'ghost';
    fly.textContent = 'Fly here';
    fly.addEventListener('click', () => view?.flyToStar(star));
    const reveal = document.createElement('button');
    reveal.type = 'button';
    reveal.className = 'ghost';
    reveal.textContent = bridge.platform === 'win32' ? 'Show in Explorer' : 'Show in folder';
    reveal.addEventListener('click', () => void bridge.reveal(info.repo.id, info.node.path));
    actions.append(fly, reveal);
    els.details.replaceChildren(header, title, path, facts, actions);
    els.details.hidden = false;
}
// ── Agents ────────────────────────────────────────────────────────────
function agentByKey(key) {
    return agents.find((a) => a.key === key);
}
let displayNames = new Map();
function agentName(a) {
    return displayNames.get(a.key) ?? (a.parentKey ? a.agentType : a.title || `Claude · ${a.sessionId.slice(0, 4)}`);
}
/** A branch change under an agent stays flagged this long. */
const BRANCH_SWITCH_SHOW_MS = 5 * 60_000;
function recentBranchSwitch(a) {
    const s = a.branchSwitch;
    return s && Date.now() - s.at < BRANCH_SWITCH_SHOW_MS ? s : null;
}
/** "salsa · feature-x": the repo, plus the branch when it's known. */
function whereText(a, repoName) {
    if (!repoName)
        return '';
    return a.branch ? `${repoName} · ${a.branch}` : repoName;
}
function fileName(path) {
    return path.includes('/') ? path.slice(path.lastIndexOf('/') + 1) : path;
}
function agentStatusText(a) {
    if (a.status === 'done')
        return 'Finished';
    if (a.status === 'waiting') {
        const n = agents.filter((s) => s.parentKey === a.key && s.status === 'working').length;
        return `Waiting on ${n} subagent${n === 1 ? '' : 's'}`;
    }
    if (a.status === 'idle')
        return 'Idle';
    if (!a.file)
        return 'Thinking…';
    const verb = { read: 'Reading', edit: 'Editing', write: 'Writing', search: 'Searching' }[a.action ?? 'read'] ?? 'Working in';
    return `${verb} ${fileName(a.file)}`;
}
/** Agents (shown in the constellation) at a path: on that file, or anywhere under that folder. */
function agentsAt(repoId, path, isDir) {
    return agents.filter((a) => {
        if (a.repoId !== repoId || a.status === 'done')
            return false;
        if (!path)
            return true;
        const f = (a.file ?? '').toLowerCase();
        const p = path.toLowerCase();
        return isDir ? f === p || f.startsWith(`${p}/`) : f === p;
    });
}
/** Agents worth listing: in one of your repos, main agents first with their subagents under them. */
function visibleAgents() {
    const inRepos = agents.filter((a) => a.repoId && repos.get(a.repoId)?.status === 'ready');
    const mains = inRepos.filter((a) => !a.parentKey);
    const out = [];
    for (const m of mains) {
        out.push(m);
        out.push(...inRepos.filter((a) => a.parentKey === m.key));
    }
    // Subagents whose parent isn't in a repo still deserve a row.
    out.push(...inRepos.filter((a) => a.parentKey && !mains.some((m) => m.key === a.parentKey)));
    return out;
}
function onAgentsChanged() {
    view?.setAgents(agents.filter((a) => a.repoId && repos.has(a.repoId)));
    renderAgentList();
    renderCollisionList();
    if (detailsAgentKey) {
        if (agentByKey(detailsAgentKey))
            showAgentDetails(detailsAgentKey);
        else
            hideDetails();
    }
}
function renderAgentList() {
    const list = visibleAgents();
    const live = list.filter((a) => a.status !== 'done').length;
    els.agentCount.textContent = live ? String(live) : '';
    if (!list.length) {
        const msg = claude?.connected
            ? 'No agents in your repos right now. Start Claude Code in one of them and it will appear here.'
            : '';
        els.agentList.replaceChildren(...(msg ? [textEl('li', msg, 'agent-empty')] : []));
        return;
    }
    els.agentList.replaceChildren(...list.map((a) => {
        const li = document.createElement('li');
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = `agent status-${a.status}${a.parentKey ? ' sub' : ''}`;
        const repo = a.repoId ? repos.get(a.repoId) : undefined;
        btn.style.setProperty('--repo-color', repo ? repoColorHex(repo.meta.colorIndex) : '#cfd6ff');
        const dot = document.createElement('span');
        dot.className = 'dot';
        const text = document.createElement('span');
        text.className = 'agent-text';
        const where = whereText(a, repo?.meta.name);
        text.append(textEl('span', agentName(a), 'agent-name'), textEl('span', `${agentStatusText(a)}${where ? ` · ${where}` : ''}`, 'agent-status'));
        const sw = recentBranchSwitch(a);
        if (sw)
            text.append(textEl('span', `Branch changed under it: ${sw.from} → ${sw.to}`, 'agent-warn'));
        if (a.windDown)
            text.append(textEl('span', WIND_DOWN_TEXT[a.windDown], 'agent-wind'));
        if (a.task && !a.parentKey)
            text.append(textEl('span', a.task, 'agent-task'));
        if (a.parentKey && a.description)
            text.append(textEl('span', a.description, 'agent-desc'));
        // Hovering a subagent shows the instructions it was given.
        if (a.parentKey && a.instructions)
            text.append(textEl('span', a.instructions, 'agent-instructions'));
        btn.append(dot, text);
        btn.addEventListener('click', () => { view?.flyToAgent(a.key); view?.selectAgent(a.key); showAgentDetails(a.key); });
        li.append(btn);
        return li;
    }));
}
async function refreshClaude() {
    try {
        claude = await bridge.claudeStatus();
    }
    catch {
        claude = null;
    }
    renderClaude();
    renderUsage();
    renderAgentList();
}
function renderClaude() {
    const box = els.claudeConnect;
    const c = claude;
    if (!c) {
        box.replaceChildren();
        return;
    }
    const parts = [];
    if (c.serverError)
        parts.push(textEl('p', `Agent tracking is off: ${c.serverError}`, 'error'));
    if (c.connected && c.outdated) {
        parts.push(textEl('p', 'Planetarium’s connection to Claude Code is from an older version. Update it to give agents Planetarium’s tools (releasing files early, and talking collisions out). Restart running Claude Code sessions afterwards so they get the tools.'));
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'primary';
        btn.textContent = 'Update connection';
        btn.addEventListener('click', () => void setClaude(true));
        parts.push(btn);
    }
    if (c.connected) {
        const row = document.createElement('div');
        row.className = 'ok';
        row.append(document.createTextNode('Connected to Claude Code'));
        const off = document.createElement('button');
        off.type = 'button';
        off.className = 'link';
        off.textContent = 'Disconnect';
        off.title = `Removes Planetarium's hooks from ${c.settingsPath}`;
        off.addEventListener('click', () => void setClaude(false));
        row.append(off);
        parts.push(row);
    }
    else {
        parts.push(textEl('p', c.partial
            ? 'Planetarium’s Claude Code hooks are incomplete. Reconnect to repair them.'
            : 'Connect Claude Code so Planetarium can show your agents. This adds Planetarium’s hooks to your Claude Code settings, plus Planetarium’s own tools for agents (releasing files, talking out collisions), which run without asking because they only talk to Planetarium. Backups are kept, and Disconnect removes all of it.'));
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'primary';
        btn.textContent = c.partial ? 'Reconnect Claude Code' : 'Connect Claude Code';
        btn.title = c.settingsPath;
        btn.addEventListener('click', () => void setClaude(true));
        parts.push(btn);
    }
    box.replaceChildren(...parts);
}
async function setClaude(on) {
    try {
        claude = on ? await bridge.claudeConnect() : await bridge.claudeDisconnect();
        showMessage(on ? 'Connected. New Claude Code activity in your repos will show up here.' : 'Disconnected from Claude Code. Your other settings were left as they were.');
    }
    catch (err) {
        showMessage(String(err), 'error');
    }
    renderClaude();
    renderAgentList();
}
function placeTooltip(x, y) {
    els.tooltip.hidden = false;
    const stage = els.canvas.getBoundingClientRect();
    const tw = els.tooltip.offsetWidth;
    const left = Math.min(x + 16, stage.width - tw - 12);
    els.tooltip.style.transform = `translate(${Math.max(8, left)}px, ${y + 18}px)`;
}
function showAgentTooltip(key, x, y) {
    const a = agentByKey(key);
    if (!a)
        return;
    const repo = a.repoId ? repos.get(a.repoId) : undefined;
    els.tooltip.style.setProperty('--repo-color', repo ? repoColorHex(repo.meta.colorIndex) : '#cfd6ff');
    const where = whereText(a, repo?.meta.name);
    const rows = [textEl('strong', agentName(a)), textEl('span', `${agentStatusText(a)}${where ? ` · ${where}` : ''}`)];
    if (a.worktree)
        rows.push(textEl('span', `In worktree ${a.worktree}`));
    const sw = recentBranchSwitch(a);
    if (sw)
        rows.push(textEl('span', `Branch changed under it: ${sw.from} → ${sw.to}`, 'warn'));
    if (a.task && !a.parentKey)
        rows.push(textEl('span', a.task));
    if (a.parentKey && a.description)
        rows.push(textEl('span', a.description, 'desc'));
    if (a.parentKey && a.instructions)
        rows.push(textEl('span', a.instructions, 'instructions'));
    els.tooltip.replaceChildren(...rows);
    placeTooltip(x, y);
}
function relativeTime(ms) {
    const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
    if (s < 60)
        return `${s}s ago`;
    const m = Math.round(s / 60);
    if (m < 60)
        return `${m} min ago`;
    return `${Math.round(m / 60)} h ago`;
}
function showAgentDetails(key) {
    const a = agentByKey(key);
    if (!a)
        return;
    detailsAgentKey = key;
    const repo = a.repoId ? repos.get(a.repoId) : undefined;
    els.details.style.setProperty('--repo-color', repo ? repoColorHex(repo.meta.colorIndex) : '#cfd6ff');
    const header = document.createElement('header');
    header.append(textEl('span', a.parentKey ? 'Subagent' : 'Agent', 'kind'), iconButton('Close', '×', hideDetails));
    const title = textEl('h3', agentName(a));
    const where = textEl('p', `${agentStatusText(a)}${repo ? ` · ${repo.meta.name}${a.file ? `/${a.file}` : ''}` : ''}`, 'path');
    const facts = document.createElement('dl');
    const addFact = (k, v) => { facts.append(textEl('dt', k), textEl('dd', v)); };
    if (a.task)
        addFact('Task', a.task);
    if (a.description)
        addFact('Doing', a.description);
    if (a.instructions)
        addFact('Instructions', a.instructions);
    if (a.branch)
        addFact('Branch', a.branch);
    if (a.worktree && a.treePath)
        addFact('Worktree', a.treePath);
    if (a.branchSwitch) {
        addFact('Branch changed', `${a.branchSwitch.from} → ${a.branchSwitch.to}, ${relativeTime(a.branchSwitch.at)} (the agent was told)`);
    }
    if (a.parentKey) {
        const parent = agentByKey(a.parentKey);
        if (parent)
            addFact('Started by', agentName(parent));
    }
    else {
        const subs = agents.filter((s) => s.parentKey === a.key);
        if (subs.length)
            addFact('Subagents', subs.map((s) => `${agentName(s)} (${agentStatusText(s).toLowerCase()})`).join(', '));
    }
    addFact('Files read', a.reads.toLocaleString());
    addFact('Edits', a.edits.toLocaleString());
    addFact('Started', relativeTime(a.startedAt));
    addFact('Last activity', relativeTime(a.lastEventAt));
    const actions = document.createElement('div');
    actions.className = 'details-actions';
    const fly = document.createElement('button');
    fly.type = 'button';
    fly.className = 'ghost';
    fly.textContent = 'Fly here';
    fly.addEventListener('click', () => view?.flyToAgent(a.key));
    actions.append(fly);
    if (a.file && a.repoId) {
        const reveal = document.createElement('button');
        reveal.type = 'button';
        reveal.className = 'ghost';
        reveal.textContent = bridge.platform === 'win32' ? 'Show file in Explorer' : 'Show file';
        reveal.addEventListener('click', () => void bridge.reveal(a.repoId, a.file, a.key));
        actions.append(reveal);
    }
    els.details.replaceChildren(header, title, where, facts, actions);
    els.details.hidden = false;
}
// ── Usage limits + "Wind down all agents" ─────────────────────────────
let usage = null;
/** Suggest winding down from this much of the session limit. */
const WIND_DOWN_SUGGEST_AT = 90;
const DEFAULT_WIND_DOWN = 'We’re at {percent}% of my session limit and my usage resets in {resets_in}. Go ahead and find a stopping point for now, and I’ll let you know when to continue working.';
const TIDY_UP = 'Before you stop: finish or undo any half-done edit so no file is left broken, make sure the project still builds if you changed code, and write 2–3 lines on what’s done and what’s next. Don’t start anything new.';
function untilText(resetsAtSecs) {
    const mins = Math.max(1, Math.ceil((resetsAtSecs * 1000 - Date.now()) / 60000));
    if (mins < 60)
        return `${mins} min`;
    if (mins < 48 * 60)
        return `${Math.floor(mins / 60)} h ${String(mins % 60).padStart(2, '0')} min`;
    return `${Math.floor(mins / (24 * 60))} days`;
}
/** Agents in the middle of something (the ones a wind-down would reach). */
function busyAgents() {
    return agents.filter((a) => a.status === 'working' || a.status === 'waiting');
}
function usageRow(label, w) {
    const row = document.createElement('div');
    const pct = Math.round(w.usedPercentage);
    row.className = `usage-row${pct >= 95 ? ' critical' : pct >= 80 ? ' high' : ''}`;
    const bar = document.createElement('div');
    bar.className = 'usage-bar';
    const fill = document.createElement('div');
    fill.style.width = `${Math.min(100, w.usedPercentage)}%`;
    bar.append(fill);
    row.append(textEl('span', label, 'usage-label'), bar, textEl('span', `${pct}% · resets in ${untilText(w.resetsAt)}`, 'usage-value'));
    return row;
}
function renderUsage() {
    const box = els.usage;
    if (!claude?.connected) {
        box.replaceChildren();
        return;
    }
    const parts = [];
    if (usage && (usage.fiveHour || usage.sevenDay)) {
        if (usage.fiveHour)
            parts.push(usageRow('Session', usage.fiveHour));
        if (usage.sevenDay)
            parts.push(usageRow('Week', usage.sevenDay));
        const age = Date.now() - usage.updatedAt;
        if (age > 10 * 60_000)
            parts.push(textEl('p', `As of ${Math.round(age / 60_000)} min ago (updates while a Claude Code session is open).`, 'usage-note'));
    }
    else if (claude.statusLine === 'theirs') {
        parts.push(textEl('p', 'Usage limits aren’t shown because you have your own Claude Code status line, which Planetarium leaves alone.', 'usage-note'));
    }
    else {
        parts.push(textEl('p', 'Your plan’s usage limits show here once a Claude Code session reports them (Pro and Max plans).', 'usage-note'));
    }
    const busy = busyAgents().length;
    const btn = document.createElement('button');
    btn.type = 'button';
    const high = (usage?.fiveHour?.usedPercentage ?? 0) >= WIND_DOWN_SUGGEST_AT && busy > 0;
    btn.className = `ghost wide wind-btn${high ? ' urgent' : ''}`;
    btn.textContent = busy ? `Wind down all agents (${busy})…` : 'Wind down all agents…';
    btn.disabled = busy === 0;
    btn.title = busy ? 'Ask every working agent to find a stopping point.' : 'No agents are working right now.';
    btn.addEventListener('click', () => openWindDown());
    parts.push(btn);
    box.replaceChildren(...parts);
}
function fillWindDown(template) {
    const w = usage?.fiveHour;
    return template
        .replaceAll('{percent}%', w ? `${Math.round(w.usedPercentage)}%` : 'close to 100%')
        .replaceAll('{percent}', w ? String(Math.round(w.usedPercentage)) : 'close to 100')
        .replaceAll('{resets_in}', w ? untilText(w.resetsAt) : 'a little while');
}
/** Turn an edited message back into a template, so saved wording keeps up-to-date numbers. */
function toTemplate(text) {
    const w = usage?.fiveHour;
    if (!w)
        return text;
    return text
        .replaceAll(`${Math.round(w.usedPercentage)}%`, '{percent}%')
        .replaceAll(untilText(w.resetsAt), '{resets_in}');
}
async function openWindDown() {
    if (!coordSettings) {
        try {
            coordSettings = await bridge.getCoordSettings();
        }
        catch { /* use the default */ }
    }
    const card = els.windDown;
    const close = () => { card.hidden = true; card.replaceChildren(); };
    const busy = busyAgents();
    const header = document.createElement('header');
    header.append(textEl('span', 'Wind down all agents', 'cc-kicker'), iconButton('Close', '×', close));
    const who = busy.length
        ? `${busy.length} agent${busy.length === 1 ? ' is' : 's are'} working right now (${busy.map(agentName).join(', ')}). Each gets this message on its next step.`
        : 'No agents are working right now.';
    const intro = textEl('p', who, 'muted');
    const label = textEl('label', 'Message', 'cc-label');
    const msg = document.createElement('textarea');
    msg.rows = 4;
    msg.value = fillWindDown(coordSettings?.windDownMessage || DEFAULT_WIND_DOWN);
    label.append(msg);
    const tidyRow = document.createElement('label');
    tidyRow.className = 'check';
    const tidy = document.createElement('input');
    tidy.type = 'checkbox';
    tidy.checked = true;
    tidyRow.append(tidy, textEl('span', 'Also ask them to leave things tidy: finish or undo half-done edits, make sure it builds, and note what’s left.'));
    const wording = document.createElement('div');
    wording.className = 'wd-wording';
    const save = document.createElement('button');
    save.type = 'button';
    save.className = 'link';
    save.textContent = 'Save this wording as my default';
    save.title = 'The percentage and reset time are filled in fresh each time.';
    save.addEventListener('click', () => {
        if (!coordSettings)
            return;
        void saveSettings({ ...coordSettings, windDownMessage: toTemplate(msg.value.trim()) }).then(() => showMessage('Saved. Your wording will be used next time.'));
    });
    const reset = document.createElement('button');
    reset.type = 'button';
    reset.className = 'link';
    reset.textContent = 'Use the default wording';
    reset.addEventListener('click', () => {
        msg.value = fillWindDown(DEFAULT_WIND_DOWN);
        if (coordSettings?.windDownMessage)
            void saveSettings({ ...coordSettings, windDownMessage: null });
    });
    wording.append(save, reset);
    const actions = document.createElement('div');
    actions.className = 'cc-actions';
    const send = document.createElement('button');
    send.type = 'button';
    send.className = 'primary';
    send.textContent = busy.length ? `Send to ${busy.length} agent${busy.length === 1 ? '' : 's'}` : 'Send';
    send.disabled = !busy.length;
    send.addEventListener('click', async () => {
        const text = msg.value.trim() + (tidy.checked ? `\n\n${TIDY_UP}` : '');
        if (!text.trim() || !bridge.windDown)
            return;
        send.disabled = true;
        try {
            const n = await bridge.windDown(text);
            showMessage(n ? `Asked ${n} agent${n === 1 ? '' : 's'} to wind down. Tell each one to continue after your usage resets.` : 'No agents were in the middle of anything.');
            close();
        }
        catch (err) {
            showMessage(String(err), 'error');
            send.disabled = false;
        }
    });
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'ghost';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', close);
    actions.append(send, cancel);
    card.replaceChildren(header, intro, label, tidyRow, wording, actions);
    card.hidden = false;
    msg.focus();
}
const WIND_DOWN_TEXT = {
    asked: 'Wind-down message waiting for its next step',
    told: 'Winding down',
    stopped: 'Stopped for the usage limit. Tell it to continue after the reset.',
};
// ── Collisions ────────────────────────────────────────────────────────
function repoName(id) {
    return repos.get(id)?.meta.name ?? 'repo';
}
function nameOf(key) {
    const a = agentByKey(key);
    return a ? agentName(a) : 'An agent';
}
function collisionStatusText(c) {
    switch (c.status) {
        case 'waiting': return 'Waiting for you';
        case 'warned': return c.severity === 'here' ? 'Warned (editing at the same time)' : 'Warned';
        case 'proceeded': return 'You let it proceed';
        case 'stopped': return c.resolution ?? 'Stopped';
        case 'expired': return 'No answer in time, warned instead';
        case 'talking': return c.rounds ? 'Agents are talking it out…' : 'Paused; asked to message the other agent';
        case 'asked_to_wait': return 'Asked to wait by the other agent';
        case 'needs_you': return 'Agents couldn’t settle it; needs you';
        case 'agreed': return c.resolution ? `Settled (${c.resolution})` : 'Settled';
        case 'no_reply': return 'No reply in time; it went ahead carefully';
        case 'no_tools': return 'Agent didn’t use the messaging tools; warned instead';
        case 'closed': return c.resolution ?? 'Closed';
    }
}
const DECISION_TEXT = { go_ahead: 'go ahead', wait: 'wait', done: 'done with the file', proceed: 'go ahead' };
/** The agents' exchange on a "Let them work it out" collision, plus buttons to step in. */
function talkDetails(c) {
    const out = [];
    if (c.messages.length) {
        const log = document.createElement('ol');
        log.className = 'talk-log';
        for (const m of c.messages.slice(-6)) {
            const li = document.createElement('li');
            const who = m.from === 'user' ? 'You' : nameOf(m.from);
            const decision = m.decision ? ` (${DECISION_TEXT[m.decision] ?? m.decision})` : '';
            li.append(textEl('span', `${who}${decision}`, 'talk-who'), textEl('span', m.text ? `“${m.text}”` : '', 'talk-text'));
            log.append(li);
        }
        out.push(log);
    }
    if (isOpenTalk(c)) {
        const row = document.createElement('div');
        row.className = 'talk-actions';
        const go = document.createElement('button');
        go.type = 'button';
        go.className = c.status === 'needs_you' ? 'primary' : 'ghost';
        go.textContent = 'Let it proceed';
        go.addEventListener('click', () => void bridge.resolveCollision(c.id, 'proceed', null, null));
        const wait = document.createElement('button');
        wait.type = 'button';
        wait.className = 'ghost';
        wait.textContent = 'Tell it to leave the file';
        wait.addEventListener('click', () => void bridge.resolveCollision(c.id, 'wait', null, null));
        row.append(go, wait);
        out.push(row);
    }
    return out;
}
function onCollisionsChanged() {
    view?.setCollisions(collisions);
    renderCollisionList();
    const waiting = collisions.filter((c) => c.status === 'waiting' && !answered.has(c.id));
    if (cardCollisionId !== null && !waiting.some((c) => c.id === cardCollisionId))
        closeCollisionCard();
    const next = waiting.find((c) => !setAside.has(c.id));
    if (cardCollisionId === null && next)
        openCollisionCard(next.id);
    else if (cardCollisionId !== null)
        updateCardQueue();
}
function renderCollisionList() {
    // Open exchanges first (they may need you), then the most recent.
    const recent = [...collisions]
        .sort((a, b) => Number(isOpenTalk(b)) - Number(isOpenTalk(a)) || b.createdAt - a.createdAt)
        .slice(0, 4);
    els.collisionList.replaceChildren(...recent.map((c) => {
        const li = document.createElement('li');
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = `collision status-${c.status}`;
        const title = textEl('span', `${fileName(c.file)} · ${repoName(c.repoId)}`, 'collision-title');
        const who = textEl('span', `${nameOf(c.agentKey)} → held by ${c.holderKeys.map(nameOf).join(', ')}`, 'collision-who');
        const status = textEl('span', collisionStatusText(c), 'collision-status');
        btn.append(title, who, status);
        btn.addEventListener('click', () => {
            if (c.status === 'waiting')
                openCollisionCard(c.id);
            const star = view?.findStar(c.repoId, c.file) ?? -1;
            if (star >= 0)
                view?.flyToStar(star);
        });
        li.className = `collision-item status-${c.status}`;
        li.append(btn, ...(c.mode === 'talk' ? talkDetails(c) : []));
        return li;
    }));
}
function agentPanel(key, role, line) {
    const a = agentByKey(key);
    const box = document.createElement('div');
    box.className = 'cc-agent';
    const repo = a?.repoId ? repos.get(a.repoId) : undefined;
    box.style.setProperty('--repo-color', repo ? repoColorHex(repo.meta.colorIndex) : '#cfd6ff');
    box.append(textEl('span', role, 'cc-role'));
    const name = document.createElement('div');
    name.className = 'cc-name';
    const dot = document.createElement('span');
    dot.className = 'dot';
    name.append(dot, document.createTextNode(a ? agentName(a) : 'Unknown agent'));
    box.append(name);
    // A subagent is described by what it was asked to do, not by its parent's task.
    const task = a?.parentKey ? (a.description ?? a.instructions ?? agentByKey(a.parentKey)?.task) : a?.task;
    box.append(textEl('p', task ? `“${task}”` : 'No task text available.', 'cc-task'));
    if (a?.parentKey && a.description && a.instructions) {
        const more = textEl('p', a.instructions, 'cc-instructions');
        more.title = a.instructions;
        box.append(more);
    }
    if (a?.parentKey) {
        const parent = agentByKey(a.parentKey);
        if (parent)
            box.append(textEl('p', `Subagent of ${agentName(parent)}`, 'cc-line'));
    }
    box.append(textEl('p', line, 'cc-line'));
    return box;
}
function updateCardQueue() {
    const more = collisions.filter((c) => c.status === 'waiting' && c.id !== cardCollisionId && !answered.has(c.id)).length;
    const q = els.collisionCard.querySelector('.cc-queue');
    if (q)
        q.textContent = more ? `+${more} more waiting` : '';
}
function openCollisionCard(id) {
    const c = collisions.find((x) => x.id === id && x.status === 'waiting' && !answered.has(x.id));
    if (!c)
        return;
    setAside.delete(id);
    cardCollisionId = id;
    const card = els.collisionCard;
    const header = document.createElement('header');
    const later = document.createElement('button');
    later.type = 'button';
    later.className = 'link';
    later.textContent = 'Later';
    later.title = 'Hide this for now. The agent keeps waiting until the timer runs out; reopen it from the Agents list.';
    later.addEventListener('click', () => { setAside.add(c.id); closeCollisionCard(); });
    header.append(textEl('span', 'Collision · needs your decision', 'cc-kicker'), textEl('span', '', 'cc-queue'), textEl('span', '', 'cc-timer'), later);
    const title = document.createElement('h3');
    title.append(document.createTextNode(c.file), textEl('span', ` in ${repoName(c.repoId)}`, 'cc-repo'));
    const holders = document.createElement('div');
    holders.className = 'cc-holders';
    for (const hk of c.holderKeys) {
        const h = agentByKey(hk);
        const also = h ? h.holds.filter((x) => x.file.toLowerCase() !== c.file.toLowerCase()).length : 0;
        const where = c.severity === 'here' && h?.file?.toLowerCase() === c.file.toLowerCase()
            ? 'Editing this file right now.'
            : 'Changed this file earlier in its current task and may come back to it.';
        holders.append(agentPanel(hk, 'Holding the file', also ? `${where} Also holding ${also} other file${also === 1 ? '' : 's'}.` : where));
    }
    const incoming = agentPanel(c.agentKey, 'About to edit it', 'Paused until you decide.');
    const columns = document.createElement('div');
    columns.className = 'cc-columns';
    columns.append(holders, incoming);
    const msgLabel = textEl('label', `Message to ${nameOf(c.agentKey)} (optional)`, 'cc-label');
    const msg = document.createElement('textarea');
    msg.rows = 2;
    msg.placeholder = 'e.g. "Only touch the shadow code; leave render() alone."';
    msgLabel.append(msg);
    const noteLabel = textEl('label', `Note for ${c.holderKeys.map(nameOf).join(', ')} (optional, delivered on its next step)`, 'cc-label');
    const note = document.createElement('input');
    note.type = 'text';
    note.placeholder = 'e.g. "Another agent will update view.ts after you; keep render()’s signature."';
    noteLabel.append(note);
    const actions = document.createElement('div');
    actions.className = 'cc-actions';
    const act = (label, action, cls, hint) => {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = cls;
        b.textContent = label;
        b.title = hint;
        b.addEventListener('click', async () => {
            actions.querySelectorAll('button').forEach((x) => (x.disabled = true));
            answered.add(c.id);
            const ok = await bridge.resolveCollision(c.id, action, msg.value.trim() || null, note.value.trim() || null);
            if (!ok)
                showMessage('That agent already moved on (the decision window had closed).', 'error');
            closeCollisionCard();
        });
        return b;
    };
    actions.append(act('Let it proceed', 'proceed', 'primary', 'The edit goes ahead; the agent is reminded to re-read the file and not undo the other agent’s work.'), act('Ask it to wait', 'wait', 'ghost', 'The edit is stopped; the agent works on other parts of its task and tries later.'), act('Check with me first', 'coordinate', 'ghost', 'The edit is stopped; the agent explains its intended change to you and waits for your go-ahead.'));
    card.replaceChildren(header, title, columns, msgLabel, noteLabel, actions);
    card.hidden = false;
    updateCardQueue();
    const star = view?.findStar(c.repoId, c.file) ?? -1;
    if (star >= 0)
        view?.flyToStar(star);
    const timer = card.querySelector('.cc-timer');
    const tick = () => {
        if (!c.deadline)
            return;
        const left = Math.max(0, Math.round((c.deadline - Date.now()) / 1000));
        timer.textContent = `${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}`;
        timer.title = 'If you don’t answer by then, the agent is warned and continues.';
    };
    tick();
    clearInterval(cardTimer);
    cardTimer = window.setInterval(tick, 1000);
}
function closeCollisionCard() {
    clearInterval(cardTimer);
    cardCollisionId = null;
    els.collisionCard.hidden = true;
    const next = collisions.find((c) => c.status === 'waiting' && !answered.has(c.id) && !setAside.has(c.id));
    if (next)
        openCollisionCard(next.id);
}
// ── Settings ──────────────────────────────────────────────────────────
const MODE_INFO = {
    off: { label: 'Off', help: 'Agents aren’t told anything. Collisions still show up here.' },
    warn: { label: 'Warn', help: 'The agent gets a note before touching a file another agent is holding, and carries on.' },
    ask: { label: 'Ask me', help: 'The edit pauses and Planetarium asks you what to do. If you don’t answer in time, the agent is warned and carries on.' },
    talk: {
        label: 'Work it out',
        help: 'Let them work it out (experimental): the edit pauses and the two agents message each other through Planetarium. The one holding the file says go ahead, wait, or that it’s done. Each exchange is capped at 3 messages; if they can’t agree it comes to you, and if nobody answers within 90 seconds the agent goes ahead carefully. You can step in from the Agents list at any time.',
    },
};
async function saveSettings(next) {
    try {
        coordSettings = await bridge.setCoordSettings(next);
    }
    catch (err) {
        showMessage(String(err), 'error');
    }
    renderSettings();
}
function modePicker(value, withDefault, onPick) {
    const group = document.createElement('div');
    group.className = 'segmented';
    const options = withDefault ? ['default', 'off', 'warn', 'ask', 'talk'] : ['off', 'warn', 'ask', 'talk'];
    for (const m of options) {
        const b = document.createElement('button');
        b.type = 'button';
        b.textContent = m === 'default' ? 'Default' : MODE_INFO[m].label;
        b.className = m === value ? 'on' : '';
        b.setAttribute('aria-pressed', String(m === value));
        if (m !== 'default')
            b.title = MODE_INFO[m].help;
        b.addEventListener('click', () => onPick(m));
        group.append(b);
    }
    return group;
}
function renderSettings() {
    const s = coordSettings;
    if (!s || els.settings.hidden)
        return;
    const header = document.createElement('header');
    header.append(textEl('h3', 'When agents collide'), iconButton('Close', '×', () => { els.settings.hidden = true; }));
    const intro = textEl('p', 'A file is “held” by an agent from the moment it edits it until that agent finishes its turn or says it’s done with it. A collision is another agent about to edit a held file.', 'muted');
    const def = document.createElement('section');
    def.append(textEl('h4', 'Default for all repos'), modePicker(s.defaultMode, false, (m) => void saveSettings({ ...s, defaultMode: m })));
    def.append(textEl('p', MODE_INFO[s.defaultMode].help, 'muted'));
    const timeout = document.createElement('section');
    timeout.append(textEl('h4', '“Ask me” waits for'));
    const sel = document.createElement('select');
    for (const secs of [60, 120, 300, 480]) {
        const o = document.createElement('option');
        o.value = String(secs);
        o.textContent = `${secs / 60} minute${secs === 60 ? '' : 's'}`;
        o.selected = s.askTimeoutSecs === secs;
        sel.append(o);
    }
    if (![60, 120, 300, 480].includes(s.askTimeoutSecs)) {
        const o = document.createElement('option');
        o.value = String(s.askTimeoutSecs);
        o.textContent = `${Math.round(s.askTimeoutSecs / 60)} minutes`;
        o.selected = true;
        sel.append(o);
    }
    sel.addEventListener('change', () => void saveSettings({ ...s, askTimeoutSecs: Number(sel.value) }));
    timeout.append(sel, textEl('p', 'After that, the agent is warned and continues, so nothing is stuck while you’re away.', 'muted'));
    const per = document.createElement('section');
    per.append(textEl('h4', 'Per repo'));
    const list = document.createElement('ul');
    list.className = 'repo-modes';
    for (const r of orderedRepos()) {
        const li = document.createElement('li');
        li.style.setProperty('--repo-color', repoColorHex(r.meta.colorIndex));
        const name = document.createElement('span');
        name.className = 'rm-name';
        const dot = document.createElement('span');
        dot.className = 'dot';
        name.append(dot, document.createTextNode(r.meta.name));
        const current = s.repoModes[r.meta.id] ?? 'default';
        li.append(name, modePicker(current, true, (m) => {
            const repoModes = { ...s.repoModes };
            if (m === 'default')
                delete repoModes[r.meta.id];
            else
                repoModes[r.meta.id] = m;
            void saveSettings({ ...s, repoModes });
        }));
        list.append(li);
    }
    if (!repos.size)
        list.append(textEl('li', 'Add a repo to set its own mode.', 'muted'));
    per.append(list);
    const parts = [header, intro, def, timeout, per];
    const uses = (m) => s.defaultMode === m || Object.values(s.repoModes).includes(m);
    if (claude?.connected && claude.outdated && (uses('ask') || uses('talk'))) {
        parts.push(textEl('p', 'Update Planetarium’s Claude Code connection (in the Agents section) so these modes work fully.', 'warn-text'));
    }
    if (uses('talk')) {
        parts.push(textEl('p', '“Work it out” needs Planetarium’s tools, which Claude Code sessions only get when they start. Restart sessions that were open before you connected (or updated) Planetarium. An agent without the tools is warned instead of stuck.', 'muted'));
    }
    els.settings.replaceChildren(...parts);
}
async function openSettings() {
    if (!coordSettings) {
        try {
            coordSettings = await bridge.getCoordSettings();
        }
        catch {
            return;
        }
    }
    els.settings.hidden = false;
    renderSettings();
}
// ── Boot ──────────────────────────────────────────────────────────────
function bindUi() {
    els.addForm.addEventListener('submit', (e) => {
        e.preventDefault();
        const value = els.pathInput.value.trim();
        if (!value) {
            void addRepo(null);
            return;
        }
        void addRepo(value);
    });
    els.browseBtn.addEventListener('click', () => void addRepo(null));
    els.emptyBrowse.addEventListener('click', () => void addRepo(null));
    els.frameBtn.addEventListener('click', () => view?.frameAll(true));
    window.addEventListener('keydown', (e) => {
        const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
        if (typing)
            return;
        if (e.key === 'f' || e.key === 'F')
            view?.frameAll(true);
        if (e.key === 'Escape') {
            if (!els.settings.hidden)
                els.settings.hidden = true;
            else
                hideDetails();
        }
    });
    // Drop a folder anywhere on the window to add it.
    let dragDepth = 0;
    window.addEventListener('dragenter', (e) => { e.preventDefault(); dragDepth++; els.dropHint.hidden = false; });
    window.addEventListener('dragleave', () => { dragDepth = Math.max(0, dragDepth - 1); if (!dragDepth)
        els.dropHint.hidden = true; });
    window.addEventListener('dragover', (e) => e.preventDefault());
    window.addEventListener('drop', (e) => {
        e.preventDefault();
        dragDepth = 0;
        els.dropHint.hidden = true;
        const files = [...(e.dataTransfer?.files ?? [])];
        void (async () => {
            for (const f of files) {
                const p = bridge.pathForFile(f);
                if (p)
                    await addRepo(p);
            }
        })();
    });
    // Tauri delivers drops as native paths rather than HTML drop events.
    bridge.onDragState?.((dragging) => { els.dropHint.hidden = !dragging; });
    bridge.onDropPaths?.((paths) => {
        els.dropHint.hidden = true;
        void (async () => { for (const p of paths)
            await addRepo(p); })();
    });
    window.addEventListener('planetarium:gpu-lost', (e) => {
        showGpuError(`The GPU connection was lost (${e.detail || 'unknown reason'}). Press Ctrl+R to reload.`);
    });
    // Keep an open details panel's tooltip anchoring honest after camera moves.
    window.addEventListener('planetarium:frame', () => {
        if (!firstFrameDone) {
            firstFrameDone = true;
            document.body.classList.add('ready');
        }
    });
}
function showGpuError(message) {
    els.gpuError.replaceChildren(textEl('h2', 'WebGPU isn’t available'), textEl('p', message), textEl('p', 'Planetarium draws the constellation with WebGPU. Updating your graphics driver usually fixes this. Your repo list still works.'));
    els.gpuError.hidden = false;
}
async function boot() {
    try {
        bridge = getBridge();
    }
    catch (err) {
        showGpuError(String(err.message));
        return;
    }
    bindUi();
    try {
        view = await ConstellationView.create(els.canvas, els.labels, {
            onHover: (star, x, y) => showTooltip(star, x, y),
            onSelect: (star) => { if (star >= 0)
                showDetails(star);
            else {
                els.details.hidden = true;
                detailsAgentKey = null;
            } },
            onHoverAgent: (key, x, y) => { if (key)
                showAgentTooltip(key, x, y); },
            onSelectAgent: (key) => showAgentDetails(key),
        });
    }
    catch (err) {
        const msg = err instanceof WebGPUUnavailableError ? err.message : `Could not start the renderer: ${String(err?.message ?? err)}`;
        showGpuError(msg);
    }
    bridge.onAgentsUpdated((list) => { agents = list; displayNames = agentDisplayNames(list); onAgentsChanged(); renderUsage(); });
    bridge.onUsageUpdated?.((u) => { usage = u; renderUsage(); });
    void bridge.getUsage?.().then((u) => { usage = u; renderUsage(); }).catch(() => undefined);
    // Keep "resets in …" current.
    setInterval(renderUsage, 30_000);
    // Nothing is drawn while the window is in the tray or minimized.
    bridge.onWindowVisible?.((visible) => view?.setPaused(!visible));
    bridge.onCollisionsUpdated((list) => { collisions = list; onCollisionsChanged(); });
    els.settingsBtn.addEventListener('click', () => { if (els.settings.hidden)
        void openSettings();
    else
        els.settings.hidden = true; });
    void refreshClaude();
    bridge.onRepoUpdated((result) => {
        const state = repos.get(result.repoId);
        if (!state)
            return;
        applyScan(state, result);
        renderRepoList();
        rebuildScene();
    });
    const list = await bridge.listRepos();
    for (const meta of list)
        repos.set(meta.id, { meta, status: 'scanning', scan: null, tree: null, layout: null, error: null });
    renderRepoList();
    els.empty.hidden = repos.size > 0;
    els.frameBtn.hidden = repos.size === 0;
    if (repos.size === 0) {
        view?.frameAll(false);
        return;
    }
    await Promise.all(orderedRepos().map(async (state) => {
        const result = await bridge.scanRepo(state.meta.id);
        applyScan(state, result);
    }));
    renderRepoList();
    rebuildScene();
    view?.frameAll(false);
    agents = await bridge.listAgents();
    displayNames = agentDisplayNames(agents);
    onAgentsChanged();
    collisions = await bridge.listCollisions();
    onCollisionsChanged();
}
void boot();
// Test hook: lets automated checks drive the view (see electron/main.cjs PLANETARIUM_EVAL).
window.__planetarium = {
    get view() { return view; },
    repos,
    showDetails,
};
