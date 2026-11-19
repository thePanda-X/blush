// App state and the WebSocket link to the hub.
//
// One socket; a browser is attached to at most one session at a time. The
// URL hash (`#/s/<hostId>`) is the source of truth for which one, so the
// back button detaches on a phone.

import { useSyncExternalStore } from "react";
import { applyPalette, savedTheme, saveTheme } from "./theme";
import type { Entry, HistoryItem, HostSummary, Palette, Status } from "./types";

export interface Session {
  hostId: string;
  /** The host hasn't sent anything yet (a session the web just opened). */
  pending: boolean;
  status: Status | null;
  order: number[];
  entries: Map<number, Entry>;
}

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error";
}

export interface State {
  link: "connecting" | "open" | "closed" | "locked";
  version: string;
  machine: string;
  themes: Record<string, Palette>;
  serverTheme: string;
  theme: string;
  hosts: HostSummary[];
  attached: string | null;
  session: Session | null;
  /** Why the attached session went away. */
  gone: { hostId: string; reason: string } | null;
  history: { items: HistoryItem[]; cursor: string | null; loading: boolean; loaded: boolean; error: string | null };
  toasts: Toast[];
}

let state: State = {
  link: "connecting",
  version: "",
  machine: "",
  themes: {},
  serverTheme: "blush",
  theme: savedTheme() ?? "blush",
  hosts: [],
  attached: null,
  session: null,
  gone: null,
  history: { items: [], cursor: null, loading: false, loaded: false, error: null },
  toasts: [],
};

const listeners = new Set<() => void>();
function set(patch: Partial<State> | ((s: State) => Partial<State>)) {
  state = { ...state, ...(typeof patch === "function" ? patch(state) : patch) };
  for (const l of listeners) l();
}
export const getState = () => state;
export function useStore<T>(pick: (s: State) => T): T {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => pick(state),
  );
}

// ---------------------------------------------------------------- toasts

let toastSeq = 0;
export function toast(text: string, kind: Toast["kind"] = "info") {
  const id = ++toastSeq;
  set((s) => ({ toasts: [...s.toasts.slice(-2), { id, text, kind }] }));
  setTimeout(() => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })), kind === "error" ? 5000 : 2800);
}

// ---------------------------------------------------------------- theme

export function setTheme(name: string | null) {
  saveTheme(name);
  const theme = name ?? state.serverTheme;
  set({ theme });
  applyPalette(state.themes[theme]);
}

// ---------------------------------------------------------------- socket

let ws: WebSocket | null = null;
let retry = 0;
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let reqSeq = 0;
const waiting = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();

function send(msg: object) {
  if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(msg));
    return true;
  }
  return false;
}

function request<T = any>(msg: object): Promise<T> {
  const req = ++reqSeq;
  return new Promise<T>((resolve, reject) => {
    waiting.set(req, { resolve, reject });
    if (!send({ ...msg, req })) {
      waiting.delete(req);
      reject(new Error("Not connected"));
    }
    setTimeout(() => {
      if (waiting.delete(req)) reject(new Error("The server didn't answer"));
    }, 90_000);
  });
}

export async function connect() {
  clearTimeout(retryTimer);
  // Ask first: a refused WebSocket doesn't say why.
  try {
    const r = await fetch("/api/me", { credentials: "same-origin", cache: "no-store" });
    if (r.status === 401) {
      set({ link: "locked" });
      return;
    }
  } catch {
    scheduleRetry();
    return;
  }
  set({ link: "connecting" });
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const sock = new WebSocket(`${proto}://${location.host}/ws`);
  ws = sock;
  sock.onopen = () => {
    retry = 0;
    set({ link: "open" });
    const id = state.attached;
    if (id) send({ t: "attach", hostId: id });
    if (state.history.loaded) loadHistory(true);
  };
  sock.onmessage = (ev) => {
    try {
      onMessage(JSON.parse(ev.data));
    } catch (e) {
      console.error(e);
    }
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    ws = null;
    if (state.link !== "locked") set({ link: "closed" });
    for (const [, w] of waiting) w.reject(new Error("Connection lost"));
    waiting.clear();
    scheduleRetry();
  };
}

function scheduleRetry() {
  clearTimeout(retryTimer);
  const delay = Math.min(8000, 500 * 2 ** retry++);
  set((s) => ({ link: s.link === "locked" ? "locked" : "closed" }));
  retryTimer = setTimeout(connect, delay);
}

// Phones suspend sockets in the background: reconnect right away on return.
if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && (!ws || ws.readyState > WebSocket.OPEN) && state.link !== "locked") {
      retry = 0;
      connect();
    }
  });
}

function onMessage(m: any) {
  if (m.req !== undefined && waiting.has(m.req)) {
    const w = waiting.get(m.req)!;
    waiting.delete(m.req);
    if (m.t === "error") w.reject(new Error(m.message));
    else w.resolve(m);
    return;
  }
  switch (m.t) {
    case "hello": {
      const theme = savedTheme() ?? m.theme;
      set({ version: m.version, machine: m.host, themes: m.themes, serverTheme: m.theme, theme });
      applyPalette(m.themes[theme] ?? m.themes[m.theme]);
      break;
    }
    case "hosts":
      set({ hosts: m.hosts });
      break;
    case "snapshot": {
      if (m.hostId !== state.attached) break;
      const entries = new Map<number, Entry>();
      for (const e of m.entries) entries.set(e.uid, e);
      set({
        session: { hostId: m.hostId, pending: !!m.pending, status: m.status, order: m.entries.map((e: Entry) => e.uid), entries },
        gone: null,
      });
      break;
    }
    case "upsert": {
      const s = state.session;
      if (!s || s.hostId !== m.hostId) break;
      const entries = new Map(s.entries);
      // New uids append, unless an `order` already placed them.
      const known = new Set(s.order);
      const added: number[] = [];
      for (const e of m.entries as Entry[]) {
        if (!known.has(e.uid)) {
          known.add(e.uid);
          added.push(e.uid);
        }
        entries.set(e.uid, e);
      }
      const order = added.length ? [...s.order, ...added] : s.order;
      set({ session: { ...s, entries, order } });
      break;
    }
    case "order": {
      const s = state.session;
      if (!s || s.hostId !== m.hostId) break;
      const keep = new Set<number>(m.uids);
      const entries = new Map([...s.entries].filter(([u]) => keep.has(u)));
      set({ session: { ...s, entries, order: m.uids } });
      break;
    }
    case "status": {
      const s = state.session;
      if (!s || s.hostId !== m.hostId) break;
      set({ session: { ...s, status: m.status, pending: false } });
      break;
    }
    case "hostGone":
      if (m.hostId === state.attached) set({ gone: { hostId: m.hostId, reason: m.reason }, session: null });
      break;
    case "history": {
      const items: HistoryItem[] = m.sessions ?? [];
      set((s) => ({
        history: {
          items: m.append ? [...s.history.items, ...items] : items,
          cursor: m.nextCursor ?? null,
          loading: false,
          loaded: true,
          error: m.error ?? null,
        },
      }));
      break;
    }
    case "error":
      toast(m.message, "error");
      break;
  }
}

// ---------------------------------------------------------------- actions

function hashHost(): string | null {
  const m = location.hash.match(/^#\/s\/([\w-]+)/);
  return m ? m[1] : null;
}

function syncFromHash() {
  const id = hashHost();
  if (id === state.attached) return;
  if (state.attached) send({ t: "detach" });
  set({ attached: id, session: id ? { hostId: id, pending: true, status: null, order: [], entries: new Map() } : null, gone: null });
  if (id) send({ t: "attach", hostId: id });
}

if (typeof window !== "undefined") {
  window.addEventListener("hashchange", syncFromHash);
  syncFromHash();
}

/** Whether the current session was opened from the list in this tab (so
 *  "back" can pop it and the phone's back stack stays tidy). */
let pushed = false;

export function attach(hostId: string) {
  const cur = hashHost();
  if (cur === hostId) return;
  if (cur) {
    // Switching sessions: replace, don't stack.
    location.replace(`#/s/${hostId}`);
  } else {
    pushed = true;
    location.hash = `#/s/${hostId}`;
  }
}

export function detach() {
  if (!hashHost()) return;
  if (pushed) {
    pushed = false;
    history.back();
  } else {
    location.hash = "#/";
  }
}

export function command(cmd: object) {
  const id = state.attached;
  if (!id) return false;
  const ok = send({ t: "cmd", hostId: id, cmd });
  if (!ok) toast("Not connected — reconnecting…", "error");
  return ok;
}

export function loadHistory(fresh = false) {
  if (state.history.loading) return;
  const cursor = fresh ? null : state.history.cursor;
  set((s) => ({ history: { ...s.history, loading: true, error: null } }));
  request({ t: "history", cursor })
    .then((m: any) => {
      if (!fresh && cursor) m.append = true;
      onMessage({ ...m, req: undefined });
    })
    .catch((e: Error) => set((s) => ({ history: { ...s.history, loading: false, error: e.message } })));
}

export async function openSession(item: { sessionId?: string; cwd: string; title?: string }) {
  const live = item.sessionId ? state.hosts.find((h) => h.sessionId === item.sessionId) : undefined;
  if (live) {
    attach(live.hostId);
    return;
  }
  const m = await request<{ hostId: string }>(
    item.sessionId ? { t: "open", sessionId: item.sessionId, cwd: item.cwd, title: item.title } : { t: "new", cwd: item.cwd },
  );
  attach(m.hostId);
}

export function closeSession(hostId: string) {
  send({ t: "close", hostId });
}

export async function unlock(token: string): Promise<string | null> {
  const r = await fetch("/api/login", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token }),
    credentials: "same-origin",
  });
  if (r.ok) {
    connect();
    return null;
  }
  const j = await r.json().catch(() => ({}));
  return j.error ?? "That didn't work";
}

export async function lock() {
  await fetch("/api/logout", { method: "POST", credentials: "same-origin" });
  ws?.close();
  set({ link: "locked" });
}
