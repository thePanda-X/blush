// The prompt box, the status row above it, and the model / mode pickers.

import { ArrowUp, ChevronDown, Square } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { elapsed, isTouch, shortMode, tokens, useNow } from "../lib/format";
import { command } from "../lib/store";
import type { Status } from "../lib/types";
import { Button, cx, Menu, MenuChoice, MenuContent, MenuLabel, MenuSep, MenuTrigger } from "./ui";

const HISTORY_KEY = "blush.history";
const loadHistory = (): string[] => {
  try {
    return JSON.parse(localStorage.getItem(HISTORY_KEY) ?? "[]");
  } catch {
    return [];
  }
};

export function StatusRow({ st }: { st: Status }) {
  const now = useNow(st.responding);
  if (st.conn === "starting")
    return (
      <Row>
        <span className="animate-pulse-soft text-accent">⁙</span>
        <span className="text-fg2">{st.connMessage || "Starting…"}</span>
      </Row>
    );
  if (st.conn === "failed" || st.conn === "exited")
    return (
      <Row>
        <span className="text-bad">✗</span>
        <span className="min-w-0 flex-1 truncate text-bad">{st.conn === "exited" ? "Claude Code exited — send /restart" : st.connMessage}</span>
      </Row>
    );
  if (st.responding) {
    const label = st.cancelling ? "Stopping…" : st.permissions.length || st.question ? "Waiting for you…" : "Responding…";
    return (
      <Row>
        <span className="animate-pulse-soft text-accent">⁙</span>
        <span className="text-fg2" data-status="responding">
          {label}
        </span>
        {st.turnStartedAt && <span className="text-muted">{elapsed(now - st.turnStartedAt)}</span>}
        {st.turnTag && <span className="text-accent/60">{st.turnTag}</span>}
        <span className="flex-1" />
        <Button size="sm" variant="ghost" className="!h-7 text-bad hover:!text-bad" onClick={() => command({ t: "cancel" })} disabled={st.cancelling}>
          <Square size={11} fill="currentColor" /> stop
        </Button>
      </Row>
    );
  }
  if (st.queuePaused && st.queue.length)
    return (
      <Row>
        <span className="text-warn">⏸</span>
        <span className="text-fg2">
          {st.queue.length} queued prompt{st.queue.length === 1 ? "" : "s"} on hold
        </span>
        <span className="flex-1" />
        <Button size="sm" variant="ghost" className="!h-7 text-accent" onClick={() => command({ t: "queue", action: "resume" })}>
          send
        </Button>
        <Button size="sm" variant="ghost" className="!h-7 text-bad" onClick={() => command({ t: "queue", action: "drop" })}>
          drop
        </Button>
      </Row>
    );
  return null;
}

function Row({ children }: { children: React.ReactNode }) {
  return <div className="flex min-h-8 items-center gap-2 px-1">{children}</div>;
}

function ContextMeter({ st }: { st: Status }) {
  if (!st.size) return null;
  const pct = st.used / st.size;
  return (
    <span className={cx(pct >= 0.75 ? "text-bad" : pct >= 0.5 ? "text-warn" : "text-dim")} title={`${Math.round(pct * 100)}% of the context window`}>
      {tokens(st.used)} / {tokens(st.size)}
    </span>
  );
}

export { ContextMeter };

function ModelMenu({ st }: { st: Status }) {
  const model = st.options.find((o) => o.id === "model" || o.category === "model");
  const effort = st.options.find((o) => o.id === "effort" || o.category === "effort");
  const fast = st.options.find((o) => o.id === "fast" || o.category === "fast");
  const legacy = !model && st.models.length > 0;
  if (!model && !effort && !legacy) return <span className="truncate text-model">{st.modelLabel}</span>;
  return (
    <Menu>
      <MenuTrigger asChild>
        <button className="flex min-w-0 items-center gap-1 truncate rounded px-1 text-model hover:bg-hover" data-menu="model">
          <span className="truncate">{st.modelLabel}</span>
          <ChevronDown size={12} className="shrink-0 opacity-60" />
        </button>
      </MenuTrigger>
      <MenuContent side="top">
        {model && (
          <>
            <MenuLabel>Model</MenuLabel>
            {model.options.map((o) => (
              <MenuChoice key={o.value} current={o.value === model.current} detail={o.description} onSelect={() => command({ t: "set_option", id: model.id, value: o.value })}>
                {o.name}
              </MenuChoice>
            ))}
          </>
        )}
        {legacy && (
          <>
            <MenuLabel>Model</MenuLabel>
            {st.models.map((m) => (
              <MenuChoice key={m.id} current={m.id === st.model} detail={m.description} onSelect={() => command({ t: "set_option", id: "__model", value: m.id })}>
                {m.name}
              </MenuChoice>
            ))}
          </>
        )}
        {effort && (
          <>
            <MenuSep />
            <MenuLabel>Effort</MenuLabel>
            {effort.options.map((o) => (
              <MenuChoice key={o.value} current={o.value === effort.current} onSelect={() => command({ t: "set_option", id: effort.id, value: o.value })}>
                {o.name}
              </MenuChoice>
            ))}
          </>
        )}
        {fast && fast.options.length > 0 && (
          <>
            <MenuSep />
            <MenuLabel>Fast mode</MenuLabel>
            {fast.options.map((o) => (
              <MenuChoice key={o.value} current={o.value === fast.current} onSelect={() => command({ t: "set_option", id: fast.id, value: o.value })}>
                {o.name}
              </MenuChoice>
            ))}
          </>
        )}
      </MenuContent>
    </Menu>
  );
}

function ModeMenu({ st }: { st: Status }) {
  if (!st.modes.length) return <span className="text-muted">{shortMode(st.mode)}</span>;
  return (
    <Menu>
      <MenuTrigger asChild>
        <button className="flex shrink-0 items-center gap-1 rounded px-1 text-fg2 hover:bg-hover" data-menu="mode">
          {shortMode(st.mode)}
          <ChevronDown size={12} className="opacity-60" />
        </button>
      </MenuTrigger>
      <MenuContent side="top">
        <MenuLabel>Permission mode</MenuLabel>
        {st.modes.map((m) => (
          <MenuChoice key={m.id} current={m.id === st.mode} detail={m.description} onSelect={() => command({ t: "set_mode", mode: m.id })}>
            {shortMode(m.id)}
          </MenuChoice>
        ))}
      </MenuContent>
    </Menu>
  );
}

export function Composer({ st, hostId }: { st: Status; hostId: string }) {
  const draftKey = `blush.draft.${hostId}`;
  const [text, setText] = useState(() => sessionStorage.getItem(draftKey) ?? "");
  const [sel, setSel] = useState(0);
  const [histIdx, setHistIdx] = useState<number | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);
  const touch = useMemo(isTouch, []);

  useEffect(() => {
    sessionStorage.setItem(draftKey, text);
  }, [text, draftKey]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.4))}px`;
  }, [text]);

  // Slash-command completion while typing the command name.
  const slash = useMemo(() => {
    const m = text.match(/^\/([\w:-]*)$/);
    if (!m) return [];
    const q = m[1].toLowerCase();
    return st.commands.filter((c) => c.name.toLowerCase().startsWith(q)).slice(0, 8);
  }, [text, st.commands]);
  useEffect(() => setSel(0), [slash.length]);

  const send = () => {
    const t = text.trim();
    if (!t) {
      if (st.queuePaused) command({ t: "queue", action: "resume" });
      return;
    }
    if (!command({ t: "prompt", text: t })) return;
    const h = loadHistory().filter((x) => x !== t);
    h.push(t);
    localStorage.setItem(HISTORY_KEY, JSON.stringify(h.slice(-100)));
    setHistIdx(null);
    setText("");
  };

  const complete = (name: string) => {
    setText(`/${name} `);
    ref.current?.focus();
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return;
    if (slash.length) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        setSel((s) => (s + (e.key === "ArrowDown" ? 1 : slash.length - 1)) % slash.length);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && slash[sel] && `/${slash[sel].name}` !== text)) {
        e.preventDefault();
        complete(slash[sel].name);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey && !e.altKey && !touch) {
      e.preventDefault();
      send();
      return;
    }
    if (e.key === "Escape" && st.responding) {
      command({ t: "cancel" });
      return;
    }
    // ↑ / ↓ on a single-line prompt walks this browser's prompt history.
    if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !text.includes("\n")) {
      const h = loadHistory();
      if (!h.length || (e.key === "ArrowDown" && histIdx === null)) return;
      if (e.key === "ArrowUp" && histIdx === null && text) return;
      e.preventDefault();
      const next = e.key === "ArrowUp" ? (histIdx === null ? h.length - 1 : Math.max(0, histIdx - 1)) : histIdx! + 1;
      if (next >= h.length) {
        setHistIdx(null);
        setText("");
      } else {
        setHistIdx(next);
        setText(h[next]);
      }
    }
  };

  const shell = text.startsWith("!");
  return (
    <div className="relative">
      {slash.length > 0 && (
        <ul className="menu absolute bottom-full left-0 mb-1 w-full max-w-md" role="listbox" data-completion>
          {slash.map((c, i) => (
            <li
              key={c.name}
              role="option"
              aria-selected={i === sel}
              data-highlighted={i === sel || undefined}
              className="menu-item"
              onMouseDown={(e) => {
                e.preventDefault();
                complete(c.name);
              }}
              onMouseEnter={() => setSel(i)}
            >
              <span className="shrink-0 text-accent">/{c.name}</span>
              <span className="truncate text-muted">{c.description}</span>
            </li>
          ))}
        </ul>
      )}
      <div className={cx("rounded-xl border bg-bg transition-colors focus-within:border-line-active", shell ? "border-cmd/60" : "border-line")}>
        <div className="flex items-start gap-2 px-3 pt-2.5">
          <span className={cx("pt-px font-bold select-none", shell ? "text-cmd" : "text-accent")}>{shell ? "!" : "❯"}</span>
          <textarea
            ref={ref}
            value={text}
            onChange={(e) => {
              setText(e.target.value);
              setHistIdx(null);
            }}
            onKeyDown={onKey}
            rows={1}
            placeholder={st.responding ? "Queue a follow-up…" : touch ? "Message Claude…" : "Message Claude…  (/ for commands, ! for shell)"}
            className="max-h-[40vh] min-h-[1.55em] flex-1 resize-none bg-transparent text-fg outline-none placeholder:text-dim"
            enterKeyHint={touch ? "enter" : "send"}
            aria-label="Prompt"
            autoComplete="off"
            autoCorrect="on"
            spellCheck
          />
        </div>
        <div className="flex items-center gap-2 px-2 pt-1 pb-2 text-[0.88em]">
          {st.queue.length > 0 && !st.queuePaused && (
            <span className="shrink-0 px-1 text-muted" data-queue>
              +{st.queue.length} queued
            </span>
          )}
          <span className="flex-1" />
          <span className="flex min-w-0 items-center gap-1">
            <ModelMenu st={st} />
            <span className="text-dim">·</span>
            <ModeMenu st={st} />
          </span>
          <button
            onClick={send}
            disabled={!text.trim() && !st.queuePaused}
            aria-label={st.responding ? "Queue prompt" : "Send"}
            title={st.responding ? "Queue (sends when Claude finishes)" : "Send"}
            className="ml-1 flex size-8 shrink-0 items-center justify-center rounded-lg bg-accent text-bg transition hover:brightness-110 disabled:bg-raised disabled:text-dim"
            data-send
          >
            <ArrowUp size={17} strokeWidth={2.5} />
          </button>
        </div>
      </div>
    </div>
  );
}
