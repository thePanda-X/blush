import * as AlertDialog from "@radix-ui/react-alert-dialog";
import { ArrowDown, ArrowLeft, MoreHorizontal, Unplug } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { closeSession, detach, toast, useStore } from "../lib/store";
import { PermissionCard, QuestionCard } from "./Cards";
import { Composer, ContextMeter, StatusRow } from "./Composer";
import { ThemeItems } from "./Sidebar";
import { EntryView } from "./Transcript";
import { Button, cx, IconButton, Menu, MenuContent, MenuItem, MenuSep, MenuTrigger } from "./ui";

export function SessionView() {
  const session = useStore((s) => s.session);
  const gone = useStore((s) => s.gone);
  const host = useStore((s) => s.hosts.find((h) => h.hostId === s.attached));
  const attached = useStore((s) => s.attached);
  const [confirmEnd, setConfirmEnd] = useState(false);

  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  // Where we last left scrollTop, so a scroll *up* reads as the user's doing.
  const lastTop = useRef(0);
  const [atBottom, setAtBottom] = useState(true);

  // Any move up detaches, however small (a trackpad nudge counts); getting
  // back near the bottom re-attaches.
  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (el.scrollTop < lastTop.current - 1 && dist > 2) follow.current = false;
    else if (dist < 32) follow.current = true;
    lastTop.current = el.scrollTop;
    setAtBottom(follow.current);
  }, []);

  const toBottom = useCallback((smooth = false) => {
    const el = scroller.current;
    follow.current = true;
    setAtBottom(true);
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
    lastTop.current = el.scrollTop;
  }, []);

  // Stay pinned to the bottom while following.
  useLayoutEffect(() => {
    if (follow.current) toBottom();
  });
  useEffect(() => {
    follow.current = true;
  }, [attached]);
  // Size changes that don't come with a render (the viewport, late layout of
  // markdown or images) keep a following view pinned too.
  useEffect(() => {
    const ro = new ResizeObserver(() => follow.current && toBottom());
    if (scroller.current) ro.observe(scroller.current);
    if (content.current) ro.observe(content.current);
    return () => ro.disconnect();
  }, [toBottom]);

  if (gone && gone.hostId === attached)
    return (
      <Centered>
        <div className="text-muted">◇ {gone.reason}</div>
        <Button variant="outline" onClick={detach} className="mt-4">
          <ArrowLeft size={15} /> Back to sessions
        </Button>
      </Centered>
    );

  const st = session?.status;
  const title = st?.title ?? host?.title ?? "New session";
  const cwdLabel = st?.cwdLabel ?? host?.cwdLabel ?? "";
  const git = st?.git ?? host?.git;
  const kind = host?.kind;

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col">
      <header className="flex shrink-0 items-center gap-1 border-b border-line px-2 py-1.5 sm:px-3" style={{ paddingTop: "max(0.375rem, env(safe-area-inset-top))" }}>
        <IconButton label="Back to sessions" onClick={detach} className="lg:hidden" data-action="back">
          <ArrowLeft size={18} />
        </IconButton>
        <div className="min-w-0 flex-1 px-1">
          <div className="truncate font-bold text-fg" data-title>
            {title}
          </div>
          <div className="flex min-w-0 items-center gap-2 text-[0.85em]">
            <span className="truncate text-muted">{cwdLabel}</span>
            {git && <span className="shrink-0 truncate text-accent/70">{git}</span>}
          </div>
        </div>
        <div className="hidden items-center gap-3 pr-1 text-[0.88em] sm:flex">
          {st && <ContextMeter st={st} />}
          {kind && <KindBadge kind={kind} />}
        </div>
        <Button variant="ghost" size="sm" onClick={detach} className="max-lg:hidden" data-action="detach">
          <Unplug size={14} /> Detach
        </Button>
        <Menu>
          <MenuTrigger asChild>
            <IconButton label="Session menu" data-action="session-menu">
              <MoreHorizontal size={18} />
            </IconButton>
          </MenuTrigger>
          <MenuContent>
            <MenuItem onSelect={detach}>
              <Unplug size={14} /> Detach
            </MenuItem>
            {st?.sessionId && (
              <MenuItem
                onSelect={() => {
                  navigator.clipboard?.writeText(`blush --resume ${st.sessionId}`).then(
                    () => toast("Copied the resume command"),
                    () => toast(`blush --resume ${st.sessionId}`),
                  );
                }}
              >
                Copy resume command
              </MenuItem>
            )}
            <MenuSep />
            <ThemeItems />
            {kind === "web" && (
              <>
                <MenuSep />
                <MenuItem danger onSelect={() => setConfirmEnd(true)}>
                  End session
                </MenuItem>
              </>
            )}
          </MenuContent>
        </Menu>
      </header>

      <div className="relative min-h-0 flex-1">
        <div ref={scroller} onScroll={onScroll} className="h-full overflow-y-auto overscroll-contain" data-scroller>
          <div ref={content} className="mx-auto w-full max-w-4xl space-y-3 px-3 py-4 sm:px-6">
            {!st && <div className="py-10 text-center text-muted animate-pulse-soft">⁙ Connecting to the session…</div>}
            {session?.order.map((uid) => {
              const e = session.entries.get(uid);
              return e ? (
                <div key={uid} data-uid={uid}>
                  <EntryView e={e} />
                </div>
              ) : null;
            })}
          </div>
        </div>
        {!atBottom && (
          <button
            onClick={() => toBottom(true)}
            className="absolute bottom-3 left-1/2 flex -translate-x-1/2 items-center gap-1 rounded-full border border-line-active bg-raised px-3 py-1 text-[0.88em] text-fg2 shadow-lg"
            data-action="jump-latest"
          >
            <ArrowDown size={13} /> Latest
          </button>
        )}
      </div>

      {st && (
        <div className="shrink-0 border-t border-line/60 px-2 pt-1 sm:px-4" style={{ paddingBottom: "max(0.5rem, env(safe-area-inset-bottom))" }}>
          <div className="mx-auto w-full max-w-4xl space-y-2">
            {st.permissions.length > 0 && (
              <PermissionCard key={JSON.stringify(st.permissions[0].id)} p={st.permissions[0]} index={0} total={st.permissions.length} />
            )}
            {st.question && <QuestionCard key={JSON.stringify(st.question.id)} q={st.question} />}
            <StatusRow st={st} />
            <Composer st={st} hostId={session!.hostId} />
            <div className="flex justify-end px-1 text-[0.85em] sm:hidden">
              <ContextMeter st={st} />
            </div>
          </div>
        </div>
      )}

      <AlertDialog.Root open={confirmEnd} onOpenChange={setConfirmEnd}>
        <AlertDialog.Portal>
          <AlertDialog.Overlay className="overlay" />
          <AlertDialog.Content className="dialog">
            <AlertDialog.Title className="font-bold text-fg">End this session?</AlertDialog.Title>
            <AlertDialog.Description className="mt-2 text-muted">
              This stops the Claude Code process the web started for it. The conversation is saved; you can open it again from Recent.
            </AlertDialog.Description>
            <div className="mt-5 flex justify-end gap-2">
              <AlertDialog.Cancel asChild>
                <Button variant="ghost">Keep it</Button>
              </AlertDialog.Cancel>
              <AlertDialog.Action asChild>
                <Button variant="danger" onClick={() => attached && closeSession(attached)} data-action="confirm-end">
                  End session
                </Button>
              </AlertDialog.Action>
            </div>
          </AlertDialog.Content>
        </AlertDialog.Portal>
      </AlertDialog.Root>
    </div>
  );
}

export function KindBadge({ kind }: { kind: "tui" | "web" }) {
  return (
    <span
      className={cx("rounded border px-1.5 text-[0.8em] leading-5", kind === "tui" ? "border-line-active text-soft" : "border-accent/40 text-accent/80")}
      title={kind === "tui" ? "Running in a terminal" : "Opened from the web"}
    >
      {kind === "tui" ? "terminal" : "web"}
    </span>
  );
}

function Centered({ children }: { children: React.ReactNode }) {
  return <div className="flex h-full flex-1 flex-col items-center justify-center p-6 text-center">{children}</div>;
}
