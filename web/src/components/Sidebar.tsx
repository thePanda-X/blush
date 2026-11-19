// The session list: live sessions (terminals and web-opened ones) and recent
// ones that can be opened.

import * as Dialog from "@radix-ui/react-dialog";
import * as Dropdown from "@radix-ui/react-dropdown-menu";
import { Lock, Palette, Plus, RefreshCw, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { ago, basename, shortMode } from "../lib/format";
import { attach, getState, loadHistory, lock, openSession, setTheme, toast, useStore } from "../lib/store";
import type { HistoryItem, HostSummary } from "../lib/types";
import { KindBadge } from "./SessionView";
import { Button, cx, IconButton, Menu, MenuChoice, MenuContent, MenuLabel, MenuTrigger } from "./ui";

export function ThemeItems() {
  const themes = useStore((s) => s.themes);
  const theme = useStore((s) => s.theme);
  const serverTheme = useStore((s) => s.serverTheme);
  return (
    <>
      <MenuLabel>Theme</MenuLabel>
      {Object.entries(themes).map(([name, p]) => (
        <MenuChoice key={name} current={name === theme} detail={p.description} onSelect={() => setTheme(name === serverTheme ? null : name)}>
          <span className="flex items-center gap-2">
            <span className="inline-block size-3 rounded-full border border-line" style={{ background: p.accent }} />
            {name}
            {name === serverTheme && <span className="text-muted">(config)</span>}
          </span>
        </MenuChoice>
      ))}
    </>
  );
}

function LinkDot() {
  const link = useStore((s) => s.link);
  const color = link === "open" ? "bg-ok" : link === "connecting" ? "bg-warn animate-pulse-soft" : "bg-bad";
  const label = { open: "Connected", connecting: "Connecting…", closed: "Reconnecting…", locked: "Locked" }[link];
  return <span className={cx("inline-block size-2 rounded-full", color)} title={label} aria-label={label} />;
}

function HostRow({ h, active }: { h: HostSummary; active: boolean }) {
  const glyph = h.attention > 0 ? "!" : "◆";
  const glyphColor = h.attention > 0 ? "text-warn" : h.responding ? "text-accent animate-pulse-soft" : h.conn === "failed" || h.conn === "exited" ? "text-bad" : "text-muted";
  return (
    <button
      onClick={() => attach(h.hostId)}
      className={cx(
        "group flex w-full items-start gap-2 rounded-md border-l-2 px-2 py-2 text-left transition-colors",
        active ? "border-accent bg-raised" : "border-transparent hover:bg-hover",
      )}
      data-host={h.hostId}
    >
      <span className={cx("w-[1.1em] shrink-0 text-center font-bold", glyphColor)}>{glyph}</span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          <span className={cx("min-w-0 flex-1 truncate", active ? "text-fg" : "text-fg2")}>{h.title || (h.pending ? "Starting…" : "New session")}</span>
          <KindBadge kind={h.kind} />
        </span>
        <span className="mt-0.5 flex min-w-0 items-center gap-2 text-[0.85em] text-muted">
          <span className="truncate">{h.cwdLabel}</span>
          {h.git && <span className="shrink-0 truncate text-accent/60">{h.git.split(":").pop()}</span>}
        </span>
        <span className="mt-0.5 block truncate text-[0.82em] text-dim">
          {h.attention > 0
            ? `needs you · ${h.attention} waiting`
            : h.responding
              ? "responding…"
              : h.conn === "starting"
                ? "starting…"
                : [h.modelLabel, h.mode && shortMode(h.mode)].filter(Boolean).join(" · ")}
        </span>
      </span>
    </button>
  );
}

function HistoryRow({ item }: { item: HistoryItem }) {
  const [busy, setBusy] = useState(false);
  const open = async () => {
    setBusy(true);
    try {
      await openSession({ sessionId: item.sessionId, cwd: item.cwd, title: item.title });
    } catch (e) {
      toast((e as Error).message, "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <button
      onClick={open}
      disabled={busy}
      className="flex w-full items-start gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-hover disabled:opacity-60"
      data-history={item.sessionId}
    >
      <span className={cx("w-[1.1em] shrink-0 text-center text-dim", busy && "animate-pulse-soft text-accent")}>◇</span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-fg2">{item.title || "Untitled session"}</span>
        <span className="mt-0.5 flex items-center gap-2 text-[0.85em] text-muted">
          <span className="min-w-0 truncate">{basename(item.cwd)}</span>
          <span className="shrink-0 text-dim">{ago(item.updatedAt)}</span>
        </span>
      </span>
    </button>
  );
}

export function NewSessionDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const hosts = useStore((s) => s.hosts);
  const history = useStore((s) => s.history.items);
  const dirs = useMemo(() => {
    const seen = new Map<string, string>();
    for (const h of hosts) if (h.cwd) seen.set(h.cwd, h.cwdLabel ?? h.cwd);
    for (const i of history) if (i.cwd && !seen.has(i.cwd)) seen.set(i.cwd, i.cwdLabel);
    return [...seen].slice(0, 8);
  }, [hosts, history]);
  const [cwd, setCwd] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setCwd((c) => c || dirs[0]?.[0] || "");
      setError(null);
    }
  }, [open, dirs]);
  const create = async () => {
    if (!cwd.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await openSession({ cwd: cwd.trim() });
      onOpenChange(false);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="overlay" />
        <Dialog.Content className="dialog" aria-describedby="new-desc">
          <div className="flex items-start justify-between gap-3">
            <Dialog.Title className="font-bold text-fg">
              <span className="text-accent">◆</span> New session
            </Dialog.Title>
            <Dialog.Close asChild>
              <IconButton label="Close" className="-mt-2 -mr-2">
                <X size={16} />
              </IconButton>
            </Dialog.Close>
          </div>
          <p id="new-desc" className="mt-1 text-muted">
            Starts Claude Code on {getState().machine || "the computer running blush"}, in this folder.
          </p>
          <form
            className="mt-4"
            onSubmit={(e) => {
              e.preventDefault();
              create();
            }}
          >
            <label htmlFor="new-cwd" className="text-fg2">
              Project folder
            </label>
            <input
              id="new-cwd"
              value={cwd}
              onChange={(e) => setCwd(e.target.value)}
              placeholder="/path/to/project"
              className="mt-1 h-10 w-full rounded-md border border-line bg-raised px-3 text-fg outline-none focus:border-accent"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
            />
            {dirs.length > 0 && (
              <div className="mt-3">
                <div className="mb-1 text-[0.85em] text-muted">Recent folders</div>
                <div className="flex flex-col">
                  {dirs.map(([full, label]) => (
                    <button
                      type="button"
                      key={full}
                      onClick={() => setCwd(full)}
                      className={cx("truncate rounded-md px-2 py-1.5 text-left hover:bg-hover", cwd === full ? "text-accent" : "text-fg2")}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {error && <div className="mt-3 text-bad">✗ {error}</div>}
            <div className="mt-5 flex justify-end gap-2">
              <Dialog.Close asChild>
                <Button type="button" variant="ghost">
                  Cancel
                </Button>
              </Dialog.Close>
              <Button type="submit" variant="primary" disabled={busy || !cwd.trim()} data-action="create-session">
                {busy ? "Starting…" : "Start session"}
              </Button>
            </div>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export function Sidebar() {
  const hosts = useStore((s) => s.hosts);
  const attached = useStore((s) => s.attached);
  const history = useStore((s) => s.history);
  const link = useStore((s) => s.link);
  const version = useStore((s) => s.version);
  const machine = useStore((s) => s.machine);
  const [newOpen, setNewOpen] = useState(false);

  useEffect(() => {
    if (link === "open" && !history.loaded && !history.loading) loadHistory(true);
  }, [link, history.loaded, history.loading]);

  const live = new Set(hosts.map((h) => h.sessionId).filter(Boolean));
  const recent = history.items.filter((i) => !live.has(i.sessionId));
  const terminals = hosts.filter((h) => h.kind === "tui");
  const web = hosts.filter((h) => h.kind === "web");

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-2 px-4 pt-3 pb-2" style={{ paddingTop: "max(0.75rem, env(safe-area-inset-top))" }}>
        <span className="text-lg font-bold text-accent">◆ blush</span>
        <span className="text-muted">web</span>
        <LinkDot />
        <span className="flex-1" />
        <Button size="sm" variant="outline" onClick={() => setNewOpen(true)} data-action="new-session">
          <Plus size={14} /> New
        </Button>
      </div>
      {machine && <div className="-mt-1 px-4 pb-2 text-[0.85em] text-dim">on {machine}</div>}

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        <Section title="Live" count={hosts.length}>
          {hosts.length === 0 && (
            <div className="px-3 py-2 text-[0.9em] text-muted">
              No sessions running. Start <span className="text-accent">blush</span> in a terminal, or open one below.
            </div>
          )}
          {[...terminals, ...web].map((h) => (
            <HostRow key={h.hostId} h={h} active={h.hostId === attached} />
          ))}
        </Section>

        <Section
          title="Recent"
          action={
            <IconButton label="Refresh" className="size-7" onClick={() => loadHistory(true)} disabled={history.loading} data-action="refresh-history">
              <RefreshCw size={13} className={cx(history.loading && "animate-spin")} />
            </IconButton>
          }
        >
          {history.error && <div className="px-3 py-2 text-[0.9em] text-bad">✗ {history.error}</div>}
          {!history.loaded && history.loading && <div className="px-3 py-2 text-muted animate-pulse-soft">Loading…</div>}
          {history.loaded && recent.length === 0 && !history.error && <div className="px-3 py-2 text-[0.9em] text-muted">Nothing else yet.</div>}
          {recent.map((i) => (
            <HistoryRow key={i.sessionId} item={i} />
          ))}
          {history.cursor && (
            <Button variant="ghost" size="sm" className="mt-1 w-full" onClick={() => loadHistory()} disabled={history.loading} data-action="more-history">
              {history.loading ? "Loading…" : "Load more"}
            </Button>
          )}
        </Section>
      </div>

      <div className="flex items-center gap-1 border-t border-line px-2 py-1.5" style={{ paddingBottom: "max(0.375rem, env(safe-area-inset-bottom))" }}>
        <Menu>
          <MenuTrigger asChild>
            <Button variant="ghost" size="sm" data-action="theme">
              <Palette size={14} /> Theme
            </Button>
          </MenuTrigger>
          <MenuContent align="start" side="top">
            <ThemeItems />
          </MenuContent>
        </Menu>
        <span className="flex-1 truncate text-center text-[0.8em] text-dim">{version && `v${version}`}</span>
        <Dropdown.Root>
          <Dropdown.Trigger asChild>
            <IconButton label="Lock this browser" data-action="lock-menu">
              <Lock size={14} />
            </IconButton>
          </Dropdown.Trigger>
          <MenuContent side="top">
            <Dropdown.Item className="menu-item" onSelect={() => lock()}>
              Lock this browser
            </Dropdown.Item>
          </MenuContent>
        </Dropdown.Root>
      </div>
      <NewSessionDialog open={newOpen} onOpenChange={setNewOpen} />
    </div>
  );
}

function Section({ title, count, action, children }: { title: string; count?: number; action?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="mt-2">
      <div className="flex h-8 items-center gap-2 px-2 text-[0.85em] text-muted">
        <span>{title}</span>
        {count !== undefined && count > 0 && <span className="text-dim">{count}</span>}
        <span className="flex-1 border-t border-line" />
        {action}
      </div>
      <div className="space-y-0.5">{children}</div>
    </section>
  );
}
