// Cards the agent waits on: permission requests and form questions.

import * as Checkbox from "@radix-ui/react-checkbox";
import * as RadioGroup from "@radix-ui/react-radio-group";
import * as Switch from "@radix-ui/react-switch";
import { Check } from "lucide-react";
import { useState } from "react";
import { command } from "../lib/store";
import type { Permission, Question, QuestionField } from "../lib/types";
import { Button, cx } from "./ui";

export function PermissionCard({ p, index, total }: { p: Permission; index: number; total: number }) {
  const [sent, setSent] = useState(false);
  const answer = (optionId: string | null) => {
    setSent(true);
    command({ t: "permission", id: p.id, optionId });
  };
  const hasReject = p.options.some((o) => o.kind.startsWith("reject"));
  return (
    <section data-card="permission" className="rounded-lg border border-line-active bg-raised p-3" aria-label="Permission request">
      <div className="flex items-baseline gap-2">
        <span className="text-warn">⚠</span>
        <span className="min-w-0 flex-1 font-bold break-words text-fg">{p.title}</span>
        {total > 1 && (
          <span className="text-[0.85em] text-muted">
            {index + 1} of {total}
          </span>
        )}
      </div>
      {p.detail.length > 0 && (
        <pre className="mt-2 max-h-48 overflow-auto rounded-md bg-bg px-3 py-2 text-[0.92em] break-words whitespace-pre-wrap">
          {p.detail.map((d, i) => (
            <div key={i} className={d.startsWith("$ ") ? "text-cmd" : "text-soft"}>
              {d}
            </div>
          ))}
        </pre>
      )}
      <div className="mt-3 flex flex-wrap gap-2">
        {p.options.map((o, i) => (
          <Button
            key={o.id}
            size="sm"
            disabled={sent}
            variant={o.kind.startsWith("reject") ? "danger" : i === 0 ? "primary" : "outline"}
            onClick={() => answer(o.id)}
            data-option={o.kind}
          >
            {o.name}
          </Button>
        ))}
        {!hasReject && (
          <Button size="sm" variant="danger" disabled={sent} onClick={() => answer(null)}>
            Reject
          </Button>
        )}
      </div>
    </section>
  );
}

type Answers = Record<string, unknown>;

const answered = (v: unknown) => (Array.isArray(v) ? v.length > 0 : typeof v === "string" ? v.trim() !== "" : v !== undefined);

export function QuestionCard({ q }: { q: Question }) {
  const [answers, setAnswers] = useState<Answers>(() => {
    const a: Answers = {};
    for (const f of q.fields) if (f.kind === "multi") a[f.key] = [];
    else if (f.kind === "bool") a[f.key] = false;
    return a;
  });
  const [sent, setSent] = useState(false);
  // Only required fields block Submit; a choice counts as answered by its "Other" text too.
  const missing = q.fields.some((f) => f.required && !answered(answers[f.key]) && !(f.other && answered(answers[f.other])));
  const reply = (action: "accept" | "decline" | "cancel") => {
    setSent(true);
    // Leave skipped fields out rather than sending empty strings and lists.
    const content = Object.fromEntries(
      Object.entries(answers)
        .filter(([, v]) => typeof v === "boolean" || answered(v))
        .map(([k, v]) => [k, typeof v === "string" ? v.trim() : v]),
    );
    command({ t: "question", id: q.id, action, content: action === "accept" ? content : undefined });
  };
  const set = (k: string, v: unknown) => setAnswers((a) => ({ ...a, [k]: v }));
  return (
    <section data-card="question" className="rounded-lg border border-line-active bg-raised p-3" aria-label="Question">
      <div className="flex items-baseline gap-2">
        <span className="text-accent">?</span>
        <span className="font-bold break-words text-fg">{q.message || "Claude has a question"}</span>
      </div>
      <div className="mt-2 max-h-[45dvh] space-y-3 overflow-y-auto pr-1">
        {q.fields.map((f) => (
          <div key={f.key} className="space-y-2">
            <Field f={f} value={answers[f.key]} onChange={(v) => set(f.key, v)} />
            {f.other && (
              <OtherBox
                id={`q-${f.other}`}
                multi={f.kind === "multi"}
                value={(answers[f.other] as string) ?? ""}
                onChange={(v) => set(f.other!, v)}
              />
            )}
          </div>
        ))}
      </div>
      <div className="mt-3 flex flex-wrap gap-2">
        <Button size="sm" variant="primary" disabled={sent || missing} onClick={() => reply("accept")}>
          Submit
        </Button>
        <Button size="sm" variant="outline" disabled={sent} onClick={() => reply("decline")}>
          Decline
        </Button>
        <Button size="sm" variant="ghost" disabled={sent} onClick={() => reply("cancel")}>
          Cancel
        </Button>
      </div>
    </section>
  );
}

/** A choice question's free-text answer: instead of a pick, or added to a multi-select's picks. */
function OtherBox({ id, multi, value, onChange }: { id: string; multi: boolean; value: string; onChange: (v: string) => void }) {
  return (
    <div className="pl-2">
      <label htmlFor={id} className="sr-only">
        Other
      </label>
      <input
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={multi ? "Other: add your own answer (optional)" : "Other: type your own answer (optional)"}
        className="h-9 w-full rounded-md border border-line bg-bg px-3 text-fg outline-none placeholder:text-muted focus:border-accent"
        autoComplete="off"
      />
    </div>
  );
}

function Preview({ text }: { text?: string | null }) {
  if (!text) return null;
  return (
    <pre data-preview className="mt-1 max-h-60 overflow-auto rounded-md bg-bg px-3 py-2 text-[0.88em] whitespace-pre text-fg2">
      {text}
    </pre>
  );
}

function Field({ f, value, onChange }: { f: QuestionField; value: unknown; onChange: (v: unknown) => void }) {
  const id = `q-${f.key}`;
  const head = (
    <div className="mb-1">
      <label htmlFor={id} className="text-fg2">
        {f.title || f.key}
      </label>
      {f.description && <div className="text-[0.88em] text-muted">{f.description}</div>}
    </div>
  );
  if (f.kind === "single")
    return (
      <div>
        <div className="flex items-start justify-between gap-2">
          {head}
          {!f.required && !!value && (
            <button type="button" onClick={() => onChange(undefined)} className="shrink-0 text-[0.88em] text-muted hover:text-fg">
              Clear
            </button>
          )}
        </div>
        <RadioGroup.Root id={id} value={(value as string) ?? ""} onValueChange={onChange} className="space-y-1" aria-label={f.title}>
          {f.options.map((o) => (
            <label key={o.value} className="flex cursor-pointer items-start gap-2 rounded-md px-2 py-1.5 hover:bg-hover">
              <RadioGroup.Item
                value={o.value}
                className="mt-[3px] flex size-4 shrink-0 items-center justify-center rounded-full border border-line-active data-[state=checked]:border-accent"
              >
                <RadioGroup.Indicator className="size-2 rounded-full bg-accent" />
              </RadioGroup.Item>
              <span className="min-w-0">
                <span className="text-fg">{o.name}</span>
                {o.description && <span className="block text-[0.88em] text-muted">{o.description}</span>}
              </span>
            </label>
          ))}
        </RadioGroup.Root>
        <Preview text={f.options.find((o) => o.value === value)?.preview} />
      </div>
    );
  if (f.kind === "multi") {
    const list = (value as string[]) ?? [];
    return (
      <div>
        {head}
        <div className="space-y-1" id={id}>
          {f.options.map((o) => {
            const on = list.includes(o.value);
            return (
              <label key={o.value} className="flex cursor-pointer items-start gap-2 rounded-md px-2 py-1.5 hover:bg-hover">
                <Checkbox.Root
                  checked={on}
                  onCheckedChange={(c) => onChange(c ? [...list, o.value] : list.filter((x) => x !== o.value))}
                  className={cx(
                    "mt-[3px] flex size-4 shrink-0 items-center justify-center rounded-[4px] border",
                    on ? "border-accent bg-accent text-bg" : "border-line-active",
                  )}
                  aria-label={o.name}
                >
                  <Checkbox.Indicator>
                    <Check size={12} strokeWidth={3} />
                  </Checkbox.Indicator>
                </Checkbox.Root>
                <span className="min-w-0 flex-1">
                  <span className="text-fg">{o.name}</span>
                  {o.description && <span className="block text-[0.88em] text-muted">{o.description}</span>}
                  {on && <Preview text={o.preview} />}
                </span>
              </label>
            );
          })}
        </div>
      </div>
    );
  }
  if (f.kind === "bool")
    return (
      <div className="flex items-center justify-between gap-3">
        <div>
          <label htmlFor={id} className="text-fg2">
            {f.title || f.key}
          </label>
          {f.description && <div className="text-[0.88em] text-muted">{f.description}</div>}
        </div>
        <Switch.Root
          id={id}
          checked={!!value}
          onCheckedChange={onChange}
          className="relative h-6 w-11 shrink-0 rounded-full border border-line-active bg-bg transition-colors data-[state=checked]:border-accent data-[state=checked]:bg-accent/30"
        >
          <Switch.Thumb className="block size-4 translate-x-1 rounded-full bg-soft transition-transform data-[state=checked]:translate-x-6 data-[state=checked]:bg-accent" />
        </Switch.Root>
      </div>
    );
  return (
    <div>
      {head}
      <input
        id={id}
        value={(value as string) ?? ""}
        onChange={(e) => onChange(e.target.value)}
        className="h-10 w-full rounded-md border border-line bg-bg px-3 text-fg outline-none focus:border-accent"
        autoComplete="off"
      />
    </div>
  );
}
