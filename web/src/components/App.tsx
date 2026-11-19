import * as Tooltip from "@radix-ui/react-tooltip";
import { useState } from "react";
import { unlock, useStore } from "../lib/store";
import { SessionView } from "./SessionView";
import { Sidebar } from "./Sidebar";
import { Button, cx } from "./ui";

export function App() {
  const link = useStore((s) => s.link);
  const attached = useStore((s) => s.attached);
  if (link === "locked") return <Login />;
  return (
    <Tooltip.Provider>
      <div className="flex h-dvh overflow-hidden">
        {/* Phones show the list or a session; desktops show both. */}
        <aside className={cx("w-full shrink-0 border-r border-line lg:block lg:w-80", attached ? "hidden" : "block")}>
          <Sidebar />
        </aside>
        <main className={cx("min-w-0 flex-1", attached ? "flex" : "hidden lg:flex")}>
          {attached ? <SessionView key={attached} /> : <Empty />}
        </main>
      </div>
      <Banner />
      <Toasts />
    </Tooltip.Provider>
  );
}

function Empty() {
  const n = useStore((s) => s.hosts.length);
  return (
    <div className="flex flex-1 flex-col items-center justify-center p-8 text-center">
      <pre className="text-[10px] leading-[1.15] text-accent select-none sm:text-xs" aria-hidden>
        {LOGO}
      </pre>
      <div className="mt-6 text-fg2">{n ? "Pick a session to attach to it." : "No sessions are running."}</div>
      <div className="mt-1 max-w-sm text-muted">
        Sessions started with <span className="text-accent">blush</span> in a terminal show up here while{" "}
        <span className="text-cmd">blush web</span> is up. You can also open a recent one, or start a new one.
      </div>
    </div>
  );
}

const LOGO = ` ███████████  ████                     █████
▒▒███▒▒▒▒▒███▒▒███                    ▒▒███
 ▒███    ▒███ ▒███  █████ ████  █████  ▒███████
 ▒██████████  ▒███ ▒▒███ ▒███  ███▒▒   ▒███▒▒███
 ▒███▒▒▒▒▒███ ▒███  ▒███ ▒███ ▒▒█████  ▒███ ▒███
 ▒███    ▒███ ▒███  ▒███ ▒███  ▒▒▒▒███ ▒███ ▒███
 ███████████  █████ ▒▒████████ ██████  ████ █████
▒▒▒▒▒▒▒▒▒▒▒  ▒▒▒▒▒   ▒▒▒▒▒▒▒▒ ▒▒▒▒▒▒  ▒▒▒▒ ▒▒▒▒▒`;

function Banner() {
  const link = useStore((s) => s.link);
  if (link === "open" || link === "connecting") return null;
  return (
    <div className="fixed top-0 right-0 left-0 z-30 flex justify-center" style={{ paddingTop: "env(safe-area-inset-top)" }}>
      <div className="mt-2 rounded-full border border-warn/40 bg-raised px-3 py-1 text-[0.85em] text-warn shadow-lg" role="status">
        <span className="animate-pulse-soft">⁙</span> Lost the connection — reconnecting…
      </div>
    </div>
  );
}

function Toasts() {
  const toasts = useStore((s) => s.toasts);
  return (
    <div className="pointer-events-none fixed bottom-24 left-1/2 z-50 flex w-[min(92vw,420px)] -translate-x-1/2 flex-col items-center gap-2" aria-live="polite">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={cx("toast rounded-lg border bg-raised px-3 py-2 text-center shadow-lg", t.kind === "error" ? "border-bad/50 text-bad" : "border-line-active text-fg2")}
        >
          {t.text}
        </div>
      ))}
    </div>
  );
}

function Login() {
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!token.trim()) return;
    setBusy(true);
    setError(await unlock(token).catch(() => "Couldn't reach the server"));
    setBusy(false);
  };
  return (
    <div className="flex h-dvh items-center justify-center p-5">
      <form onSubmit={submit} className="w-full max-w-sm rounded-xl border border-line bg-raised p-5" aria-label="Unlock">
        <div className="text-lg font-bold text-accent">◆ blush web</div>
        <p className="mt-2 text-muted">
          Open the link printed by <span className="text-cmd">blush web up</span>, or paste it (or its token) here.
        </p>
        <input
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder="token or link"
          className="mt-4 h-10 w-full rounded-md border border-line bg-bg px-3 text-fg outline-none focus:border-accent"
          autoComplete="off"
          autoCapitalize="off"
          spellCheck={false}
          aria-label="Token"
          autoFocus
        />
        {error && <div className="mt-2 text-bad">✗ {error}</div>}
        <Button type="submit" variant="primary" className="mt-4 w-full" disabled={busy || !token.trim()}>
          {busy ? "Checking…" : "Unlock"}
        </Button>
      </form>
    </div>
  );
}
