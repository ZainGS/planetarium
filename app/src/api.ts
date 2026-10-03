/** api.ts — types for `window.planetarium`, the bridge defined in web/bridge.js. */

export interface RepoMeta {
  id: string;
  path: string;
  name: string;
  colorIndex: number;
  addedAt: number;
}

export interface ScanResult {
  ok: boolean;
  repoId: string;
  files: string[];
  truncated: boolean;
  method: 'git' | 'walk';
  gitRoots: string[];
  scannedAt: number;
  error?: string;
}

export interface AddResult {
  ok: boolean;
  repo?: RepoMeta;
  canceled?: boolean;
  error?: string;
  code?: string;
  repoId?: string;
}

export type AgentStatus = 'working' | 'idle' | 'waiting' | 'done';

/** A coding agent as tracked by the Rust side (src-tauri/src/agents.rs). */
export interface AgentInfo {
  key: string;
  sessionId: string;
  agentId: string | null;
  /** 'main' for a session's own agent, else the subagent type (Explore, Plan, …). */
  agentType: string;
  parentKey: string | null;
  repoId: string | null;
  /** Repo-relative path with forward slashes. */
  file: string | null;
  action: 'read' | 'edit' | 'write' | 'search' | null;
  status: AgentStatus;
  task: string | null;
  /** Subagents: the short description it was started with, and its instructions (shortened). */
  description: string | null;
  instructions: string | null;
  title: string | null;
  startedAt: number;
  lastEventAt: number;
  lastFileAt: number;
  edits: number;
  reads: number;
  /** Files this agent edited during its current turn and may come back to. */
  holds: { repoId: string; file: string; since: number }[];
  /** The git checkout it's working in (the folder), and the branch checked out there. */
  treePath: string | null;
  branch: string | null;
  /** Set when that checkout is a linked git worktree: the worktree folder's name. */
  worktree: string | null;
  /** The latest time the branch changed under it (someone ran git checkout/switch). */
  branchSwitch: { from: string; to: string; at: number } | null;
  /** "Wind down all agents": message waiting ("asked"), received ("told"), turn ended after it ("stopped"). */
  windDown: 'asked' | 'told' | 'stopped' | null;
}

/** Your Claude plan's usage limits, as Claude Code reports them (src-tauri/src/usage.rs). */
export interface UsageWindow { usedPercentage: number; resetsAt: number }
export interface Usage { fiveHour: UsageWindow | null; sevenDay: UsageWindow | null; updatedAt: number }

export type CollisionMode = 'off' | 'warn' | 'ask' | 'talk';

/** One agent about to touch a file other agents are holding (src-tauri/src/agents.rs). */
export interface Collision {
  id: number;
  repoId: string;
  file: string;
  /** The agent about to touch the file. */
  agentKey: string;
  /** The agents holding it. */
  holderKeys: string[];
  /** 'here': a holder is in the file right now; 'held': a holder changed it earlier this turn. */
  severity: 'here' | 'held';
  access: 'edit' | 'read';
  mode: 'warn' | 'ask' | 'talk';
  /** warn/ask: warned … expired. talk ("Let them work it out"): talking … closed. */
  status: 'warned' | 'waiting' | 'proceeded' | 'stopped' | 'expired'
    | 'talking' | 'asked_to_wait' | 'needs_you' | 'agreed' | 'no_reply' | 'no_tools' | 'closed';
  createdAt: number;
  resolvedAt: number | null;
  resolution: string | null;
  deadline: number | null;
  /** Talk mode: what the agents said to each other ("user" = you). */
  messages: { from: string; text: string; decision: 'go_ahead' | 'wait' | 'done' | 'proceed' | null; at: number }[];
  rounds: number;
}

/**
 * Display names for agents. Subagents are named by type ("general-purpose"); when several
 * live ones share a type, each gets a short id so you can tell them apart ("general-purpose · 3f9a").
 */
export function agentDisplayNames(list: AgentInfo[]): Map<string, string> {
  const base = (a: AgentInfo) => (a.parentKey ? a.agentType : a.title || `Claude · ${a.sessionId.slice(0, 4)}`);
  const counts = new Map<string, number>();
  for (const a of list) if (a.status !== 'done') counts.set(base(a), (counts.get(base(a)) ?? 0) + 1);
  const out = new Map<string, string>();
  for (const a of list) {
    const b = base(a);
    out.set(a.key, a.parentKey && (counts.get(b) ?? 0) > 1 && a.agentId ? `${b} · ${a.agentId.slice(0, 4)}` : b);
  }
  return out;
}

/** A talk-mode collision that's still being worked out (or waiting for you). */
export function isOpenTalk(c: Collision): boolean {
  return c.mode === 'talk' && (c.status === 'talking' || c.status === 'asked_to_wait' || c.status === 'needs_you');
}

export interface CoordSettings {
  defaultMode: CollisionMode;
  repoModes: Record<string, CollisionMode>;
  askTimeoutSecs: number;
  /** Your own wording for "Wind down all agents"; null = the default. */
  windDownMessage?: string | null;
}

export interface ClaudeInfo {
  connected: boolean;
  partial: boolean;
  settingsPath: string;
  hookUrl: string;
  serverError: string | null;
  /** Connected with hooks from an older Planetarium; reconnecting updates them. */
  outdated: boolean;
  /** Planetarium's status line (usage data) is installed, you have your own, or there's none. */
  statusLine?: 'ours' | 'theirs' | 'none';
}

export interface PlanetariumBridge {
  listRepos(): Promise<RepoMeta[]>;
  addRepo(folder?: string | null): Promise<AddResult>;
  removeRepo(id: string): Promise<boolean>;
  scanRepo(id: string): Promise<ScanResult>;
  /** With an agent key, an agent in a git worktree has its own copy of the file shown. */
  reveal(id: string, relPath: string, agentKey?: string): Promise<boolean>;
  pathForFile(file: File): string | null;
  onRepoUpdated(cb: (scan: ScanResult) => void): () => void;
  /**
   * Optional: hosts whose webview swallows file drops (Tauri) report dropped paths and drag
   * state here instead of through HTML drag events.
   */
  onDropPaths?(cb: (paths: string[]) => void): void;
  onDragState?(cb: (dragging: boolean) => void): void;
  listAgents(): Promise<AgentInfo[]>;
  onAgentsUpdated(cb: (agents: AgentInfo[]) => void): () => void;
  claudeStatus(): Promise<ClaudeInfo>;
  claudeConnect(): Promise<ClaudeInfo>;
  claudeDisconnect(): Promise<ClaudeInfo>;
  listCollisions(): Promise<Collision[]>;
  onCollisionsUpdated(cb: (list: Collision[]) => void): () => void;
  getUsage?(): Promise<Usage | null>;
  onUsageUpdated?(cb: (u: Usage) => void): () => void;
  /** Ask every agent mid-turn to wind down; resolves to how many were asked. */
  windDown?(message: string): Promise<number>;
  /** false while the window is hidden in the tray or minimized. */
  onWindowVisible?(cb: (visible: boolean) => void): () => void;
  resolveCollision(id: number, action: 'proceed' | 'wait' | 'coordinate', message: string | null, noteForHolders: string | null): Promise<boolean>;
  getCoordSettings(): Promise<CoordSettings>;
  setCoordSettings(value: CoordSettings): Promise<CoordSettings>;
  platform: string;
}

declare global {
  interface Window { planetarium?: PlanetariumBridge }
}

export function getBridge(): PlanetariumBridge {
  if (!window.planetarium) throw new Error('Planetarium bridge missing: this page must run inside the Planetarium app.');
  return window.planetarium;
}
