import { useEffect, useState } from "react";

export function duration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))}ms`;
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)}s`;
  if (s < 60) return `${Math.round(s)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${Math.round(s % 60)}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** A live timer: whole seconds, like the TUI's spinner row. */
export function elapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function tokens(n: number): string {
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}K`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

export function ago(iso: string | number | null): string {
  if (iso === null) return "";
  const t = typeof iso === "number" ? iso : Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  if (s < 86400 * 7) return `${Math.floor(s / 86400)}d ago`;
  return new Date(t).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

/** The current time, re-rendering every `every` ms while `on`. */
export function useNow(on: boolean, every = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!on) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), every);
    return () => clearInterval(t);
  }, [on, every]);
  return now;
}

export function shortMode(id: string): string {
  return (
    { bypassPermissions: "always-approve", default: "ask", acceptEdits: "accept-edits", plan: "plan", auto: "auto", dontAsk: "dont-ask" }[
      id
    ] ?? id
  );
}

export function basename(p: string | null | undefined): string {
  if (!p) return "";
  const parts = p.replace(/\\/g, "/").replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || p;
}

export const isTouch = () => typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
