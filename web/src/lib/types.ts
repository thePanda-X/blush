// Shapes sent by the blush web hub (see src/web/sync.rs and src/web/hub.rs).

export type Palette = Record<string, string> & { description?: string; light?: boolean };

export interface HostSummary {
  hostId: string;
  kind: "tui" | "web";
  pid: number;
  pending: boolean;
  sessionId: string | null;
  title: string | null;
  cwd: string | null;
  cwdLabel: string | null;
  git: string | null;
  conn: "starting" | "ready" | "failed" | "exited";
  responding: boolean;
  attention: number;
  modelLabel: string | null;
  mode: string | null;
}

export interface HistoryItem {
  sessionId: string;
  cwd: string;
  cwdLabel: string;
  title: string;
  updatedAt: string | null;
}

export interface Choice {
  value: string;
  name: string;
  description: string;
}

export interface ConfigOption {
  id: string;
  name: string;
  category: string;
  current: string;
  options: Choice[];
}

export interface Permission {
  id: unknown;
  title: string;
  detail: string[];
  options: { id: string; name: string; kind: string }[];
}

export interface QuestionField {
  key: string;
  title: string;
  description: string;
  kind: "single" | "multi" | "text" | "bool";
  options: Choice[];
}

export interface Question {
  id: unknown;
  message: string;
  fields: QuestionField[];
}

export interface Status {
  sessionId: string | null;
  title: string | null;
  cwd: string;
  cwdLabel: string;
  git: string | null;
  conn: "starting" | "ready" | "failed" | "exited";
  connMessage: string;
  agent: string;
  responding: boolean;
  cancelling: boolean;
  turnStartedAt: number | null;
  turnTag: string | null;
  mode: string;
  modes: { id: string; name: string; description: string }[];
  model: string;
  models: { id: string; name: string; description: string }[];
  modelLabel: string;
  options: ConfigOption[];
  used: number;
  size: number;
  cost: number | null;
  queue: string[];
  queuePaused: boolean;
  permissions: Permission[];
  question: Question | null;
  commands: { name: string; description: string; hint: string }[];
  busy: boolean;
}

/** `[kind, old line, new line, text]`; kind is " ", "+", "-" or "~" (gap). */
export type DiffLine = [string, number | null, number | null, string];

export type ToolContent =
  | { k: "text"; text: string }
  | { k: "terminal"; text: string }
  | { k: "diff"; path: string; created: boolean; lines: DiffLine[] };

interface Base {
  uid: number;
  at: number;
}

export type Entry = Base &
  (
    | { k: "welcome" }
    | { k: "user"; text: string; queued: boolean; tag: string | null }
    | { k: "agent"; text: string }
    | { k: "thought"; text: string; startedAt: number; ms: number | null }
    | {
        k: "tool";
        id: string;
        title: string;
        kind: string;
        status: "pending" | "running" | "done" | "failed";
        content: ToolContent[];
        input: {
          command: string | null;
          description: string | null;
          path: string | null;
          pattern: string | null;
          url: string | null;
          query: string | null;
          full: string;
        } | null;
        locations: string[];
        startedAt: number;
        ms: number | null;
      }
    | { k: "plan"; items: { content: string; status: string }[] }
    | { k: "notice"; level: "info" | "warn" | "error" | "success"; text: string }
    | { k: "turnEnd"; reason: string }
    | { k: "aside"; question: string; answer: string; startedAt: number; ms: number | null; failed: boolean }
  );

export type EntryOf<K extends Entry["k"]> = Extract<Entry, { k: K }>;
