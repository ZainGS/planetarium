// bridge.js — gives the shared Planetarium page the same `window.planetarium` API it gets from
// Electron's preload, implemented with Tauri's invoke/events. Loaded before the app's module.
(() => {
  const T = window.__TAURI__;
  if (!T || !T.core) {
    console.error('[planetarium] Tauri globals missing; is app.withGlobalTauri enabled?');
    return;
  }
  const invoke = T.core.invoke;

  function listen(name, cb) {
    let unlisten = null;
    let stopped = false;
    T.event.listen(name, (e) => cb(e.payload)).then((fn) => {
      if (stopped) fn(); else unlisten = fn;
    });
    return () => { stopped = true; if (unlisten) unlisten(); };
  }

  // Tauri hands file drops to us as native paths (HTML drop events don't carry them here).
  const dropHandlers = [];
  const dragHandlers = [];
  function onDrag(payload) {
    const type = payload && payload.type;
    if (type === 'enter' || type === 'over') dragHandlers.forEach((h) => h(true));
    else if (type === 'leave') dragHandlers.forEach((h) => h(false));
    else if (type === 'drop') {
      dragHandlers.forEach((h) => h(false));
      const paths = (payload && payload.paths) || [];
      if (paths.length) dropHandlers.forEach((h) => h(paths));
    }
  }
  const webview = T.webview && T.webview.getCurrentWebview && T.webview.getCurrentWebview();
  if (webview && webview.onDragDropEvent) {
    webview.onDragDropEvent((e) => onDrag(e.payload));
  } else {
    listen('tauri://drag-enter', (p) => onDrag({ ...p, type: 'enter' }));
    listen('tauri://drag-leave', () => onDrag({ type: 'leave' }));
    listen('tauri://drag-drop', (p) => onDrag({ ...p, type: 'drop' }));
  }

  const ua = navigator.userAgent;
  const platform = /Windows/.test(ua) ? 'win32' : /Mac OS X/.test(ua) ? 'darwin' : 'linux';

  window.planetarium = {
    listRepos: () => invoke('list_repos'),
    addRepo: (folder) => invoke('add_repo', { folder: folder || null }),
    removeRepo: (id) => invoke('remove_repo', { id }),
    scanRepo: (id) => invoke('scan_repo', { id }),
    reveal: (id, relPath, agentKey) => invoke('reveal', { id, relPath, agentKey: agentKey || null }),
    pathForFile: () => null,
    onRepoUpdated: (cb) => listen('repos:updated', cb),
    onDropPaths: (cb) => { dropHandlers.push(cb); },
    onDragState: (cb) => { dragHandlers.push(cb); },
    listAgents: () => invoke('list_agents'),
    onAgentsUpdated: (cb) => listen('agents:updated', cb),
    claudeStatus: () => invoke('claude_status'),
    claudeConnect: () => invoke('claude_connect'),
    claudeDisconnect: () => invoke('claude_disconnect'),
    listCollisions: () => invoke('list_collisions'),
    onCollisionsUpdated: (cb) => listen('collisions:updated', cb),
    resolveCollision: (id, action, message, noteForHolders) =>
      invoke('resolve_collision', { id, action, message: message || null, noteForHolders: noteForHolders || null }),
    getCoordSettings: () => invoke('get_coord_settings'),
    setCoordSettings: (value) => invoke('set_coord_settings', { value }),
    onWindowVisible: (cb) => listen('window:visible', cb),
    getUsage: () => invoke('get_usage'),
    onUsageUpdated: (cb) => listen('usage:updated', cb),
    windDown: (message) => invoke('wind_down', { message }),
    platform,
  };
})();
