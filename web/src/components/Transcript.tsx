// The scrollback: each entry rendered the way the TUI draws it.

import * as Collapsible from "@radix-ui/react-collapsible";
import { memo, useMemo, useState } from "react";
import { clock, duration, elapsed, useNow } from "../lib/format";
import { highlight, langOf, renderMarkdown } from "../lib/markdown";
import type { DiffLine, Entry, EntryOf } from "../lib/types";
import { cx, Diamond } from "./ui";

const VERBS = new Set(["Read", "Edit", "Write", "Fetch", "Search", "Find", "List", "Grep", "Glob", "Delete", "Move", "Create", "Update", "Insert", "Replace", "Open", "View", "Run"]);

export const EntryView = memo(function EntryView({ e }: { e: Entry }) {
  switch (e.k) {
    case "welcome":
      return null;
    case "user":
      return <UserBlock e={e} />;
    case "agent":
      return <Markdown text={e.text} />;
    case "thought":
      return <ThoughtBlock e={e} />;
    case "tool":
      return <ToolBlock e={e} />;
    case "plan":
      return <PlanBlock e={e} />;
    case "notice":
      return <NoticeBlock e={e} />;
    case "turnEnd":
      return (
        <div className="flex text-muted italic">
          <Diamond hollow className="text-dim not-italic" />
          {e.reason}
        </div>
      );
    case "aside":
      return <AsideBlock e={e} />;
  }
});

function Markdown({ text, className }: { text: string; className?: string }) {
  const html = useMemo(() => renderMarkdown(text), [text]);
  return <div className={cx("md", className)} dangerouslySetInnerHTML={{ __html: html }} />;
}

function UserBlock({ e }: { e: EntryOf<"user"> }) {
  return (
    <div
      data-entry="user"
      className={cx("-mx-3 border-l-2 bg-raised px-3 py-2.5 sm:-mx-4 sm:px-4", e.queued ? "border-dim" : "border-accent")}
    >
      <div className="flex items-start gap-2">
        <span className={cx("font-bold", e.queued ? "text-dim" : "text-accent")}>❯</span>
        <div className={cx("min-w-0 flex-1 font-bold break-words whitespace-pre-wrap", e.queued ? "text-muted" : "text-fg")}>
          {e.text.trimEnd()}
          {e.tag && <span className="ml-2 font-normal text-accent/60">{e.tag}</span>}
          {e.queued && <span className="ml-2 font-normal text-dim italic">queued</span>}
        </div>
        <span className="shrink-0 pt-px text-[0.85em] text-dim">{clock(e.at)}</span>
      </div>
    </div>
  );
}

function ThoughtBlock({ e }: { e: EntryOf<"thought"> }) {
  const live = e.ms === null;
  const now = useNow(live);
  const [open, setOpen] = useState(false);
  const has = e.text.trim().length > 0;
  return (
    <Collapsible.Root open={open && has} onOpenChange={setOpen}>
      <Collapsible.Trigger
        disabled={!has}
        className="flex w-full items-baseline text-left disabled:cursor-default"
        data-entry="thought"
      >
        <Diamond live={live} className={live ? "text-think" : "text-muted"} />
        <span className="text-fg2">{live ? "Thinking…" : "Thought"}</span>
        <span className="ml-1 text-muted">{live ? elapsed(now - e.startedAt) : `for ${duration(e.ms ?? 0)}`}</span>
        {has && <span className="ml-2 text-dim">{open ? "▾" : "▸"}</span>}
      </Collapsible.Trigger>
      <Collapsible.Content>
        <div className="mt-1 pl-[1.1em] whitespace-pre-wrap text-soft italic">{e.text.trim()}</div>
      </Collapsible.Content>
    </Collapsible.Root>
  );
}

function diffCounts(e: EntryOf<"tool">): [number, number] | null {
  let ins = 0;
  let del = 0;
  let any = false;
  for (const c of e.content) {
    if (c.k !== "diff") continue;
    any = true;
    for (const l of c.lines) {
      if (l[0] === "+") ins++;
      else if (l[0] === "-") del++;
    }
  }
  return any ? [ins, del] : null;
}

function ToolLabel({ e }: { e: EntryOf<"tool"> }) {
  const title = (e.title.split("\n")[0] ?? "").trim().replace(/`/g, "");
  if (e.kind === "execute") {
    const cmd = e.input?.command?.split("\n")[0] ?? title;
    return (
      <>
        <span className="font-bold text-fg">Run </span>
        <span className="text-cmd">{cmd}</span>
      </>
    );
  }
  const sp = title.indexOf(" ");
  const verb = sp > 0 ? title.slice(0, sp) : "";
  if (VERBS.has(verb)) {
    const pathy = ["read", "edit", "delete", "move"].includes(e.kind);
    return (
      <>
        <span className="font-bold text-fg">{verb} </span>
        <span className={pathy ? "text-path" : "text-fg2"}>{title.slice(sp + 1)}</span>
      </>
    );
  }
  return <span className="font-bold text-fg">{title || "Tool"}</span>;
}

function stripFences(s: string) {
  const t = s.trim();
  if (t.startsWith("```")) {
    const nl = t.indexOf("\n");
    return (nl >= 0 ? t.slice(nl + 1) : "").replace(/```\s*$/, "").trimEnd();
  }
  return s;
}

function ToolBlock({ e }: { e: EntryOf<"tool"> }) {
  const live = e.status === "pending" || e.status === "running";
  const failed = e.status === "failed";
  const now = useNow(live);
  const counts = diffCounts(e);
  const hasDiff = counts !== null;
  const texts = e.content.filter((c) => c.k !== "diff" && stripFences(c.text).trim());
  const foldable = texts.length > 0 || !!e.input?.full;
  const [open, setOpen] = useState(false);
  const took = live ? now - e.startedAt : e.ms;
  return (
    <div data-entry="tool" data-status={e.status}>
      <button
        type="button"
        onClick={() => foldable && setOpen((o) => !o)}
        className={cx("flex w-full min-w-0 items-baseline text-left", !foldable && "cursor-default")}
        aria-expanded={foldable ? open : undefined}
      >
        <Diamond live={live} className={failed ? "text-bad" : live ? "text-accent" : "text-muted"} />
        <span className="min-w-0 flex-1 truncate">
          <ToolLabel e={e} />
        </span>
        <span className="ml-2 shrink-0 whitespace-nowrap">
          {counts && (
            <>
              <span className="text-ins">+{counts[0]}</span> <span className="text-del">-{counts[1]}</span>
            </>
          )}
          {took !== null && (live || took >= 1000) && <span className="ml-2 text-dim">{live ? elapsed(took) : duration(took)}</span>}
          {failed && <span className="ml-2 text-bad">failed</span>}
          {foldable && <span className="ml-2 text-dim">{open ? "▾" : "▸"}</span>}
        </span>
      </button>
      {e.kind === "execute" && e.input?.description && <div className="truncate pl-[1.1em] text-muted">{e.input.description}</div>}
      {e.content.map((c, i) => (c.k === "diff" ? <Diff key={i} path={c.path} lines={c.lines} full={open} /> : null))}
      {(open || (failed && texts.length > 0)) && (
        <div className="mt-1 space-y-1 pl-[1.1em]">
          {texts.map((c, i) => (
            <pre
              key={i}
              className={cx(
                "max-h-[50vh] overflow-auto rounded-md border border-line bg-raised px-3 py-2 text-[0.92em] break-words whitespace-pre-wrap",
                failed ? "text-bad" : "text-soft",
              )}
            >
              {stripFences(c.k === "diff" ? "" : c.text)}
            </pre>
          ))}
          {open && texts.length === 0 && !hasDiff && e.input?.full && (
            <pre className="overflow-auto rounded-md border border-line bg-raised px-3 py-2 text-[0.92em] whitespace-pre-wrap text-muted">{e.input.full}</pre>
          )}
        </div>
      )}
    </div>
  );
}

function Diff({ path, lines, full }: { path: string; lines: DiffLine[]; full: boolean }) {
  const lang = langOf(path);
  const shown = full ? lines : lines.slice(0, 14);
  const width = String(lines.reduce((m, l) => Math.max(m, l[1] ?? 0, l[2] ?? 0), 0)).length;
  return (
    <div className="mt-1 ml-[1.1em] overflow-x-auto rounded-md border border-line text-[0.9em] leading-[1.45]" data-diff>
      <table className="w-full border-collapse">
        <tbody>
          {shown.map((l, i) => {
            const [k, o, n, text] = l;
            if (k === "~")
              return (
                <tr key={i}>
                  <td colSpan={3} className="bg-raised px-2 text-center text-dim select-none">
                    {text || "⋯"}
                  </td>
                </tr>
              );
            const no = k === "-" ? o : n;
            return (
              <tr key={i} className={k === "+" ? "bg-ins-bg" : k === "-" ? "bg-del-bg" : ""}>
                <td className="w-0 px-2 text-right whitespace-nowrap text-dim select-none" style={{ minWidth: `${width + 1}ch` }}>
                  {no ?? ""}
                </td>
                <td className={cx("w-0 pr-1 select-none", k === "+" ? "text-ins" : k === "-" ? "text-del" : "text-dim")}>{k === " " ? " " : k}</td>
                <td className="pr-3 whitespace-pre" dangerouslySetInnerHTML={{ __html: highlight(text, lang) || " " }} />
              </tr>
            );
          })}
        </tbody>
      </table>
      {!full && lines.length > shown.length && (
        <div className="border-t border-line bg-raised px-2 py-0.5 text-dim">… {lines.length - shown.length} more lines</div>
      )}
    </div>
  );
}

function PlanBlock({ e }: { e: EntryOf<"plan"> }) {
  const done = e.items.filter((i) => i.status === "completed").length;
  return (
    <div data-entry="plan">
      <div className="flex items-baseline">
        <Diamond className="text-plan" />
        <span className="font-bold text-fg">Plan</span>
        <span className="ml-2 text-muted">
          {done}/{e.items.length} done
        </span>
      </div>
      <ul className="mt-0.5 pl-[1.1em]">
        {e.items.map((it, i) => (
          <li key={i} className="flex gap-2">
            <span className={it.status === "completed" ? "text-ok" : it.status === "in_progress" ? "text-accent" : "text-dim"}>
              {it.status === "completed" ? "✓" : it.status === "in_progress" ? "◐" : "○"}
            </span>
            <span className={it.status === "completed" ? "text-muted line-through decoration-dim" : it.status === "in_progress" ? "text-fg" : "text-fg2"}>
              {it.content}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function NoticeBlock({ e }: { e: EntryOf<"notice"> }) {
  const [glyph, color] = {
    info: ["◇", "text-muted"],
    warn: ["⚠", "text-warn"],
    error: ["✗", "text-bad"],
    success: ["✓", "text-ok"],
  }[e.level];
  const qr = e.text.includes("█");
  return (
    <div className="flex" data-entry="notice" data-level={e.level}>
      <span className={cx("inline-block w-[1.1em] shrink-0", color)}>{glyph}</span>
      <div className={cx("min-w-0 flex-1 break-words whitespace-pre-wrap", e.level === "info" ? "text-muted" : color, qr && "leading-none")}>
        {e.text}
      </div>
    </div>
  );
}

function AsideBlock({ e }: { e: EntryOf<"aside"> }) {
  const live = e.ms === null;
  const now = useNow(live);
  return (
    <div className="rounded-md border border-line px-3 py-2" data-entry="aside">
      <div className="flex items-baseline gap-1">
        <Diamond live={live} className={e.failed ? "text-bad" : "text-info"} />
        <span className="font-bold text-info">btw</span>
        <span className="min-w-0 flex-1 truncate text-fg2">{e.question}</span>
        <span className="text-dim">{live ? elapsed(now - e.startedAt) : duration(e.ms ?? 0)}</span>
      </div>
      {e.answer && <Markdown text={e.answer} className={cx("mt-1", e.failed && "text-bad")} />}
    </div>
  );
}
