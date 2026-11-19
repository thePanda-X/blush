// A scripted ACP agent for testing blush web without a real model.
//
// Point blush at it with `agent.command = "bun"` and
// `agent.args = ["<path>/mock-agent.ts"]` (see run.ts). What a prompt does
// depends on the words in it:
//
//   "perm"  asks permission to run a command
//   "ask"   asks a form question (select, multi-select, text, yes/no)
//   "slow"  streams for ~8s, so it can be stopped
//   other   a full turn: thinking, a read, an edit with a diff, a plan,
//           and a markdown reply
//
// Sessions persist to $MOCK_STATE (a JSON file) so `session/load` can replay
// them in another process.

import { appendFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";

type Json = any;
const STATE = process.env.MOCK_STATE ?? "";
const LOG = process.env.MOCK_LOG ?? "";

interface Stored {
  sessionId: string;
  cwd: string;
  title: string;
  updatedAt: string;
  turns: { user: string; agent: string }[];
}

function load(): Record<string, Stored> {
  if (STATE && existsSync(STATE)) return JSON.parse(readFileSync(STATE, "utf8"));
  return {};
}
function save(all: Record<string, Stored>) {
  if (STATE) writeFileSync(STATE, JSON.stringify(all, null, 2));
}
function log(m: string) {
  if (LOG) appendFileSync(LOG, `[${process.pid}] ${m}\n`);
}

let nextId = 1000;
const pending = new Map<number, (v: Json) => void>();
const send = (m: Json) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n");
const notify = (sessionId: string, update: Json) => send({ method: "session/update", params: { sessionId, update } });
const request = (method: string, params: Json) =>
  new Promise<Json>((resolve) => {
    const id = nextId++;
    pending.set(id, resolve);
    send({ id, method, params });
  });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const modes = (current: string) => ({
  availableModes: [
    { id: "bypassPermissions", name: "Bypass Permissions", description: "Skip all permission prompts" },
    { id: "default", name: "Default", description: "Ask before edits and commands" },
    { id: "acceptEdits", name: "Accept Edits", description: "Auto-accept file edits" },
    { id: "plan", name: "Plan Mode", description: "Research first, then propose" },
  ],
  currentModeId: current,
});

const config: Record<string, string> = {};
function configOptions(sid: string) {
  const c = (k: string, d: string) => config[`${sid}:${k}`] ?? d;
  return [
    {
      id: "model",
      name: "Model",
      category: "model",
      type: "select",
      currentValue: c("model", "default"),
      options: [
        { value: "default", name: "Default", description: "Opus 5.5" },
        { value: "sonnet", name: "Sonnet", description: "Sonnet 5.5" },
        { value: "haiku", name: "Haiku", description: "Haiku 5.5" },
      ],
    },
    {
      id: "effort",
      name: "Effort",
      category: "effort",
      type: "select",
      currentValue: c("effort", "high"),
      options: ["low", "medium", "high", "max"].map((v) => ({ value: v, name: v[0].toUpperCase() + v.slice(1) })),
    },
  ];
}

const cancelled = new Set<string>();
const modeOf = new Map<string, string>();

function sessionResult(sid: string) {
  return { sessionId: sid, modes: modes(modeOf.get(sid) ?? "default"), configOptions: configOptions(sid) };
}

function commands(sid: string) {
  notify(sid, {
    sessionUpdate: "available_commands_update",
    availableCommands: [
      { name: "compact", description: "Clear history but keep a summary in context", input: { hint: "instructions" } },
      { name: "review", description: "Review a pull request" },
    ],
  });
}

const say = async (sid: string, text: string, chunk = 18, delay = 15) => {
  for (let i = 0; i < text.length; i += chunk) {
    notify(sid, { sessionUpdate: "agent_message_chunk", content: { type: "text", text: text.slice(i, i + chunk) } });
    await sleep(delay);
  }
};

const REPLY = `## Done

I added **\`div\`** to \`calc.py\`; it raises on zero:

\`\`\`python
def div(a, b):
    if b == 0:
        raise ZeroDivisionError("b is zero")
    return a / b
\`\`\`

| Function | Raises |
|---|---|
| \`div\` | \`ZeroDivisionError\` |

- Tests pass
- [x] Documented
> Note: <script>alert(1)</script> stays text.

See [the docs](https://example.com).`;

async function prompt(sid: string, text: string): Promise<Json> {
  const all = load();
  const s = all[sid];
  const lower = text.toLowerCase();
  let reply = "";
  cancelled.delete(sid);

  if (lower.includes("perm")) {
    const tc = "tool-perm-" + nextId++;
    notify(sid, {
      sessionUpdate: "tool_call",
      toolCallId: tc,
      title: "rm -rf build",
      kind: "execute",
      status: "pending",
      rawInput: { command: "rm -rf build", description: "Remove the build folder" },
    });
    const r = await request("session/request_permission", {
      sessionId: sid,
      toolCall: { toolCallId: tc, title: "rm -rf build", rawInput: { command: "rm -rf build", description: "Remove the build folder" } },
      options: [
        { optionId: "allow", name: "Allow", kind: "allow_once" },
        { optionId: "always", name: "Always allow", kind: "allow_always" },
        { optionId: "reject", name: "Reject", kind: "reject_once" },
      ],
    });
    const outcome = r?.outcome?.outcome === "selected" ? r.outcome.optionId : r?.outcome?.outcome ?? "none";
    log(`permission -> ${outcome}`);
    const ok = outcome === "allow" || outcome === "always";
    notify(sid, {
      sessionUpdate: "tool_call_update",
      toolCallId: tc,
      status: ok ? "completed" : "failed",
      content: [{ type: "content", content: { type: "text", text: ok ? "removed build/" : "Permission denied" } }],
    });
    reply = `Permission answer: ${outcome}`;
    await say(sid, reply);
  } else if (lower.includes("ask")) {
    const r = await request("elicitation/create", {
      sessionId: sid,
      mode: "form",
      message: "How should I set up the project?",
      requestedSchema: {
        type: "object",
        properties: {
          lang: {
            type: "string",
            title: "Language",
            description: "Pick one",
            oneOf: [
              { const: "ts", title: "TypeScript" },
              { const: "rs", title: "Rust" },
            ],
          },
          extras: {
            type: "array",
            title: "Extras",
            items: { anyOf: [{ const: "lint", title: "Linting" }, { const: "ci", title: "CI" }, { const: "docs", title: "Docs" }] },
          },
          name: { type: "string", title: "Project name" },
          git: { type: "boolean", title: "Initialize git?" },
        },
      },
    });
    log(`question -> ${JSON.stringify(r)}`);
    reply = `Answer: ${JSON.stringify(r)}`;
    await say(sid, reply, 40);
  } else if (lower.includes("slow")) {
    notify(sid, { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "Taking my time." } });
    for (let i = 0; i < 40; i++) {
      if (cancelled.has(sid)) {
        log("cancelled");
        return { stopReason: "cancelled" };
      }
      await say(sid, `tick ${i} `, 50, 0);
      await sleep(200);
    }
    reply = "slow done";
  } else {
    notify(sid, { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "The user wants a **div** function. " } });
    await sleep(60);
    notify(sid, { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: "I'll read calc.py first." } });
    await sleep(60);
    notify(sid, {
      sessionUpdate: "plan",
      entries: [
        { content: "Read calc.py", status: "completed", priority: "medium" },
        { content: "Add div()", status: "in_progress", priority: "medium" },
        { content: "Run the tests", status: "pending", priority: "medium" },
      ],
    });
    notify(sid, {
      sessionUpdate: "tool_call",
      toolCallId: "read-" + nextId,
      title: "Read calc.py",
      kind: "read",
      status: "completed",
      locations: [{ path: s ? `${s.cwd}/calc.py` : "calc.py" }],
      rawInput: { file_path: "calc.py" },
      content: [{ type: "content", content: { type: "text", text: "def add(a, b):\n    return a + b\n" } }],
    });
    await sleep(60);
    const ed = "edit-" + nextId++;
    notify(sid, { sessionUpdate: "tool_call", toolCallId: ed, title: "Edit calc.py", kind: "edit", status: "in_progress", rawInput: { file_path: "calc.py" } });
    await sleep(80);
    notify(sid, {
      sessionUpdate: "tool_call_update",
      toolCallId: ed,
      status: "completed",
      locations: [{ path: "calc.py", line: 10 }],
      content: [
        {
          type: "diff",
          path: "calc.py",
          oldText: "def add(a, b):\n    return a + b\n",
          newText: 'def add(a, b):\n    return a + b\n\ndef div(a, b):\n    if b == 0:\n        raise ZeroDivisionError("b is zero")\n    return a / b\n',
        },
      ],
    });
    const bash = "bash-" + nextId++;
    notify(sid, {
      sessionUpdate: "tool_call",
      toolCallId: bash,
      title: "pytest -q",
      kind: "execute",
      status: "completed",
      rawInput: { command: "pytest -q", description: "Run the tests" },
      content: [{ type: "content", content: { type: "text", text: "....\n4 passed in 0.02s" } }],
    });
    notify(sid, {
      sessionUpdate: "plan",
      entries: [
        { content: "Read calc.py", status: "completed", priority: "medium" },
        { content: "Add div()", status: "completed", priority: "medium" },
        { content: "Run the tests", status: "completed", priority: "medium" },
      ],
    });
    reply = REPLY;
    await say(sid, reply);
  }

  notify(sid, { sessionUpdate: "usage_update", used: 31000 + (s?.turns.length ?? 0) * 1200, size: 1000000, cost: { amount: 0.12, currency: "USD" } });
  if (s) {
    if (!s.turns.length) {
      s.title = text.slice(0, 40);
      notify(sid, { sessionUpdate: "session_info_update", title: s.title });
    }
    s.turns.push({ user: text, agent: reply });
    s.updatedAt = new Date().toISOString();
    save(all);
  }
  return { stopReason: "end_turn", usage: { inputTokens: 1200, outputTokens: 340, totalTokens: 1540 } };
}

async function handle(msg: Json) {
  const { id, method, params } = msg;
  if (method === undefined && id !== undefined) {
    pending.get(id)?.(msg.result ?? { error: msg.error });
    pending.delete(id);
    return;
  }
  log(`<- ${method}`);
  const ok = (result: Json) => send({ id, result });
  switch (method) {
    case "initialize":
      return ok({
        protocolVersion: 1,
        agentCapabilities: { loadSession: true, sessionCapabilities: { list: {}, close: {} }, promptCapabilities: { image: true } },
        agentInfo: { name: "mock-agent", title: "Mock Agent", version: "1.0" },
      });
    case "session/new": {
      const sid = "mock-" + Math.random().toString(36).slice(2, 8);
      const all = load();
      all[sid] = { sessionId: sid, cwd: params.cwd, title: "", updatedAt: new Date().toISOString(), turns: [] };
      save(all);
      ok(sessionResult(sid));
      setTimeout(() => commands(sid), 20);
      return;
    }
    case "session/load": {
      const sid = params.sessionId;
      const s = load()[sid];
      if (!s) return send({ id, error: { code: -32002, message: `no session ${sid}` } });
      for (const t of s.turns) {
        notify(sid, { sessionUpdate: "user_message_chunk", content: { type: "text", text: t.user } });
        notify(sid, { sessionUpdate: "agent_message_chunk", content: { type: "text", text: t.agent } });
      }
      if (s.title) notify(sid, { sessionUpdate: "session_info_update", title: s.title });
      await sleep(30);
      ok(sessionResult(sid));
      setTimeout(() => commands(sid), 20);
      return;
    }
    case "session/list": {
      const list = Object.values(load())
        .filter((s) => s.turns.length && (!params?.cwd || params.cwd === s.cwd))
        .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
        .map((s) => ({ sessionId: s.sessionId, cwd: s.cwd, title: s.title, updatedAt: s.updatedAt }));
      return ok({ sessions: list });
    }
    case "session/set_mode":
      modeOf.set(params.sessionId, params.modeId);
      ok({});
      notify(params.sessionId, { sessionUpdate: "current_mode_update", currentModeId: params.modeId });
      return;
    case "session/set_config_option":
      config[`${params.sessionId}:${params.configId}`] = String(params.value);
      return ok({ configOptions: configOptions(params.sessionId) });
    case "session/prompt": {
      const text = (params.prompt ?? []).filter((b: Json) => b.type === "text").map((b: Json) => b.text).join("\n");
      log(`prompt: ${text}`);
      return ok(await prompt(params.sessionId, text));
    }
    case "session/close":
    case "session/delete":
      return ok({});
    case "session/cancel":
      cancelled.add(params.sessionId);
      return;
    default:
      if (id !== undefined) send({ id, error: { code: -32601, message: `unknown ${method}` } });
  }
}

let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d: string) => {
  buf += d;
  let i: number;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i).trim();
    buf = buf.slice(i + 1);
    if (line) handle(JSON.parse(line)).catch((e) => log(`error: ${e}`));
  }
});
process.stdin.on("end", () => process.exit(0));
