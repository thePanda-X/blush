// End-to-end test for blush web, run with Node (`node e2e/run.ts`).
//
// Everything runs against an isolated BLUSH_HOME whose agent is the scripted
// mock in mock-agent.ts, so no model is called. It:
//   1. starts the server with `blush web up` and checks `blush web` status,
//   2. starts a real blush TUI in a hidden console (a terminal session to attach to),
//   3. drives every web input in Chromium at desktop and Pixel 8 sizes,
//      saving screenshots to e2e/out/,
//   4. stops the server with `blush web down`.
//
// Needs a debug build (`cargo build`) and `bun` on PATH (for the mock agent).

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, type Browser, type BrowserContext, type Page } from "playwright";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, "../..");
const TMP = join(HERE, ".tmp");
const OUT = join(HERE, "out");
const HOME = join(TMP, "home");
const PROJ = join(TMP, "proj");
const STATE = join(TMP, "mock-state.json");
const MOCK_LOG = join(TMP, "mock.log");
const EXE = join(ROOT, "target", "debug", process.platform === "win32" ? "blush.exe" : "blush");
const PORT = 4799;
const env = { ...process.env, BLUSH_HOME: HOME, BLUSH_NO_UPDATE: "1" };

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const blush = (...args: string[]) => spawnSync(EXE, args, { env, encoding: "utf8", timeout: 30_000 });

// ---------------------------------------------------------------- setup

function setup() {
  if (!existsSync(EXE)) throw new Error(`build blush first: ${EXE}`);
  blush("web", "down");
  rmSync(TMP, { recursive: true, force: true });
  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(HOME, { recursive: true });
  mkdirSync(PROJ, { recursive: true });
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(PROJ, "calc.py"), "def add(a, b):\n    return a + b\n");
  try {
    execFileSync("git", ["init", "-q", "-b", "main"], { cwd: PROJ });
  } catch {
    /* the header just won't show a branch */
  }
  const toml = (s: string) => JSON.stringify(s.replace(/\\/g, "/"));
  writeFileSync(
    join(HOME, "config.toml"),
    `[ui]
theme = "blush"

[agent]
default_mode = "default"
command = "bun"
args = [${toml(join(HERE, "mock-agent.ts"))}]

[agent.env]
MOCK_STATE = ${toml(STATE)}
MOCK_LOG = ${toml(MOCK_LOG)}

[features]
narrator = false
sub_meter = false
timeline = false
notify = false

[update]
auto = false

[web]
port = ${PORT}
bind = "0.0.0.0"
share = true
`,
  );
  // One past session for "Recent".
  writeFileSync(
    STATE,
    JSON.stringify({
      "mock-past01": {
        sessionId: "mock-past01",
        cwd: PROJ,
        title: "Refactor the parser",
        updatedAt: new Date(Date.now() - 3 * 3600_000).toISOString(),
        turns: [{ user: "refactor the parser into modules", agent: "Split **parser.rs** into `lexer.rs` and `ast.rs`." }],
      },
    }),
  );
}

let tuiPid = 0;
function startTui() {
  if (process.platform !== "win32") throw new Error("the hidden-console TUI launcher is Windows-only");
  const ps = `$p = Start-Process -FilePath '${EXE}' -WorkingDirectory '${PROJ}' -WindowStyle Hidden -PassThru; $p.Id`;
  const out = execFileSync("powershell", ["-NoProfile", "-Command", ps], { env, encoding: "utf8" });
  tuiPid = Number(out.trim());
}
function stopTui() {
  if (tuiPid) spawnSync("taskkill", ["/PID", String(tuiPid), "/T", "/F"]);
}

// ---------------------------------------------------------------- steps

const results: { name: string; ok: boolean; err?: string }[] = [];
let current: Page | null = null;
async function step(name: string, fn: () => Promise<void>) {
  const t = Date.now();
  try {
    await fn();
    results.push({ name, ok: true });
    console.log(`  ✓ ${name} (${Date.now() - t}ms)`);
  } catch (e) {
    const err = String((e as Error)?.message ?? e).split("\n").slice(0, 4).join("\n");
    results.push({ name, ok: false, err });
    console.log(`  ✗ ${name}\n      ${err.replace(/\n/g, "\n      ")}`);
    await current?.screenshot({ path: join(OUT, `FAIL-${name.replace(/[^\w]+/g, "_")}.png`) }).catch(() => {});
    // Close any menu or dialog left open so the next step starts clean.
    await current?.keyboard.press("Escape").catch(() => {});
    await current?.keyboard.press("Escape").catch(() => {});
  }
}
function assert(c: unknown, msg: string): asserts c {
  if (!c) throw new Error(msg);
}
async function shot(page: Page, name: string, fullPage = false) {
  await sleep(250);
  await page.screenshot({ path: join(OUT, `${name}.png`), fullPage });
}
const mockLog = () => (existsSync(MOCK_LOG) ? readFileSync(MOCK_LOG, "utf8") : "");

async function sendPrompt(page: Page, text: string) {
  const box = page.locator("textarea[aria-label=Prompt]");
  await box.fill(text);
  await page.locator("[data-send]").click();
  await page.waitForFunction(() => !(document.querySelector("textarea[aria-label=Prompt]") as HTMLTextAreaElement).value);
}
async function waitIdle(page: Page, timeout = 20_000) {
  await page.waitForSelector("[data-status=responding]", { state: "detached", timeout });
}
async function lastAgentText(page: Page) {
  return (await page.locator(".md").last().textContent()) ?? "";
}
async function pickMenu(page: Page, trigger: string, item: string | RegExp) {
  await page.locator(trigger).click();
  await page.getByRole("menuitem").filter({ hasText: item }).first().click();
}

// ---------------------------------------------------------------- main

async function main() {
  console.log("setup");
  setup();

  let token = "";
  let lanUrl = "";
  await step("cli: `blush web` reports not running", async () => {
    const r = blush("web");
    assert(r.status === 0, `exit ${r.status}`);
    assert(/isn't running/.test(r.stdout), r.stdout);
  });
  await step("cli: `blush web up` starts the server and prints links + QR", async () => {
    const r = blush("web", "up");
    assert(r.status === 0, `exit ${r.status}: ${r.stderr}`);
    writeFileSync(join(OUT, "cli-web-up.txt"), r.stdout);
    const urls = [...r.stdout.matchAll(/http:\/\/[^\s]+\?token=(\w+)/g)];
    assert(urls.length > 0, "no URL printed");
    token = urls[0][1];
    lanUrl = urls[0][0];
    assert(r.stdout.includes("█"), "no QR code");
  });
  await step("cli: `blush web up` again says already running", async () => {
    const r = blush("web", "up");
    assert(/already running/.test(r.stdout), r.stdout);
  });
  await step("cli: `blush web` shows status", async () => {
    const r = blush("web", "status");
    assert(/is running on port 4799/.test(r.stdout), r.stdout);
    writeFileSync(join(OUT, "cli-web-status.txt"), r.stdout);
  });

  startTui();
  console.log(`  (TUI pid ${tuiPid})`);

  const exe = process.env.PW_CHROMIUM ?? join(process.env.LOCALAPPDATA ?? "", "ms-playwright", "chromium-1248", "chrome-win64", "chrome.exe");
  const browser: Browser = await chromium.launch(existsSync(exe) ? { executablePath: exe } : {});
  const base = `http://localhost:${PORT}`;

  // ------------------------------------------------ desktop
  console.log("desktop (1440×900)");
  const desk: BrowserContext = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
  const page = await desk.newPage();
  current = page;
  const consoleErrors: string[] = [];
  page.on("pageerror", (e) => consoleErrors.push(String(e)));
  page.on("console", (m) => m.type() === "error" && consoleErrors.push(m.text()));

  await step("locked without a token", async () => {
    await page.goto(base);
    await page.getByRole("form", { name: "Unlock" }).waitFor();
    await shot(page, "desktop-01-locked");
  });
  await step("wrong token is refused", async () => {
    await page.getByLabel("Token").fill("nope");
    await page.getByRole("button", { name: "Unlock" }).click();
    await page.getByText("doesn't match").waitFor();
  });
  await step("pasting the link unlocks", async () => {
    await page.getByLabel("Token").fill(`${base}/?token=${token}`);
    await page.getByRole("button", { name: "Unlock" }).click();
    await page.getByText("◆ blush").first().waitFor();
  });
  await step("API rejects requests without the token", async () => {
    const r = await fetch(`${base}/api/status`);
    assert(r.status === 401, `status ${r.status}`);
    const ok = await fetch(`${base}/api/status`, { headers: { "x-blush-token": token } });
    assert(ok.status === 200, `status ${ok.status}`);
  });
  await step("assets are served gzipped", async () => {
    const r = await fetch(`${base}/index.js`, { headers: { "accept-encoding": "gzip" } });
    assert(r.headers.get("content-encoding") === "gzip", `encoding ${r.headers.get("content-encoding")}`);
  });

  await step("terminal session shows up under Live", async () => {
    await page.locator("[data-host]").first().waitFor({ timeout: 30_000 });
    await page.waitForFunction(() => document.querySelector("[data-host]")?.textContent?.includes("terminal"));
    await page.getByText("Refactor the parser").waitFor();
    await shot(page, "desktop-02-list");
  });

  await step("attach to the terminal session", async () => {
    await page.locator("[data-host]").first().click();
    await page.locator("textarea[aria-label=Prompt]").waitFor();
    await page.waitForFunction(() => location.hash.startsWith("#/s/"));
    // Wait for the session to be ready (mode menu populated).
    await page.locator("[data-menu=mode]").waitFor({ timeout: 30_000 });
  });

  await step("send a prompt: full turn renders (thinking, plan, tools, diff, markdown)", async () => {
    await sendPrompt(page, "add a div function that raises on zero");
    await page.locator("[data-entry=user]").filter({ hasText: "add a div function" }).waitFor();
    await waitIdle(page);
    await page.getByText("Done").first().waitFor();
    assert(await page.locator("[data-entry=tool]").count() >= 3, "tools missing");
    assert(await page.locator("[data-diff]").count() >= 1, "diff missing");
    assert(await page.locator(".md table").count() >= 1, "table missing");
    assert(await page.locator(".md pre code .hljs-keyword").count() >= 1, "code not highlighted");
    assert(await page.locator("[data-entry=plan]").count() >= 1, "plan missing");
    const scripts = await page.locator(".md script").count();
    assert(scripts === 0, "raw HTML rendered");
    assert((await lastAgentText(page)).includes("<script>alert(1)</script>"), "raw HTML not shown as text");
    await page.waitForFunction(() => document.querySelector("[data-title]")?.textContent?.includes("add a div"));
    await shot(page, "desktop-03-turn");
  });

  await step("fold / unfold a tool and a thought", async () => {
    const run = page.locator("[data-entry=tool]").filter({ hasText: "pytest" }).locator("button").first();
    await run.click();
    await page.getByText("4 passed").waitFor();
    await run.click();
    await page.getByText("4 passed").waitFor({ state: "detached" });
    const thought = page.locator("[data-entry=thought]").first();
    await thought.click();
    await page.getByText("I'll read calc.py first.").waitFor();
    await shot(page, "desktop-04-unfolded");
    await thought.click();
  });

  await step("permission mode menu", async () => {
    await pickMenu(page, "[data-menu=mode]", "plan");
    await page.waitForFunction(() => document.querySelector("[data-menu=mode]")?.textContent?.includes("plan"));
    await pickMenu(page, "[data-menu=mode]", "accept-edits");
    await page.waitForFunction(() => document.querySelector("[data-menu=mode]")?.textContent?.includes("accept-edits"));
  });

  await step("model and effort menu", async () => {
    await page.locator("[data-menu=model]").click();
    await shot(page, "desktop-05-model-menu");
    await page.getByRole("menuitem").filter({ hasText: "Sonnet" }).first().click();
    await page.waitForFunction(() => document.querySelector("[data-menu=model]")?.textContent?.includes("Sonnet"));
    await pickMenu(page, "[data-menu=model]", /^Max$/);
    await page.waitForFunction(() => document.querySelector("[data-menu=model]")?.textContent?.includes("(max)"));
  });

  await step("slash command completion (arrows, Tab, click)", async () => {
    const box = page.locator("textarea[aria-label=Prompt]");
    await box.fill("/co");
    await page.locator("[data-completion]").waitFor();
    const items = await page.locator("[data-completion] [role=option]").allTextContents();
    assert(items.some((t) => t.includes("/compact")) && items.some((t) => t.includes("/commit")), items.join("|"));
    await shot(page, "desktop-06-slash");
    await box.press("ArrowDown");
    await box.press("Tab");
    const v = await box.inputValue();
    assert(/^\/co\w+ $/.test(v), `completed to ${v}`);
    await box.fill("/ren");
    await page.locator("[data-completion] [role=option]").first().click();
    assert((await box.inputValue()) === "/rename ", await box.inputValue());
    await box.fill("");
  });

  await step("Enter sends, Shift+Enter adds a line", async () => {
    const box = page.locator("textarea[aria-label=Prompt]");
    await box.click();
    await box.pressSequentially("line one");
    await box.press("Shift+Enter");
    await box.pressSequentially("line two");
    assert((await box.inputValue()) === "line one\nline two", JSON.stringify(await box.inputValue()));
    await box.press("Enter");
    await page.locator("[data-entry=user]").filter({ hasText: "line two" }).waitFor();
    await waitIdle(page);
  });

  await step("↑ recalls the last prompt", async () => {
    const box = page.locator("textarea[aria-label=Prompt]");
    await box.click();
    await box.press("ArrowUp");
    assert((await box.inputValue()).includes("line two"), await box.inputValue());
    await box.fill("");
  });

  await step("permission request: Allow", async () => {
    await sendPrompt(page, "perm check one");
    await page.locator("[data-card=permission]").waitFor();
    await page.locator("[data-host]").filter({ hasText: "needs you" }).waitFor();
    await shot(page, "desktop-07-permission");
    await page.locator("[data-card=permission] [data-option=allow_once]").click();
    await page.locator("[data-card=permission]").waitFor({ state: "detached" });
    await waitIdle(page);
    assert(mockLog().includes("permission -> allow"), "agent didn't get allow");
    assert((await lastAgentText(page)).includes("allow"), await lastAgentText(page));
  });

  await step("permission request: Reject", async () => {
    await sendPrompt(page, "perm check two");
    await page.locator("[data-card=permission] [data-option=reject_once]").click();
    await waitIdle(page);
    assert(mockLog().includes("permission -> reject"), "agent didn't get reject");
    await page.locator("[data-entry=tool][data-status=failed]").last().waitFor();
  });

  await step("question form: radio, checkboxes, text, switch, submit", async () => {
    await sendPrompt(page, "ask me how to set up");
    const card = page.locator("[data-card=question]");
    await card.waitFor();
    await card.getByText("Rust").click();
    await card.getByRole("checkbox", { name: "Linting" }).click();
    await card.getByRole("checkbox", { name: "Docs" }).click();
    await card.locator("input#q-name").fill("blushy");
    await card.getByRole("switch").click();
    await shot(page, "desktop-08-question");
    await card.getByRole("button", { name: "Submit" }).click();
    await waitIdle(page);
    const txt = await lastAgentText(page);
    for (const want of ['"lang":"rs"', '"lint"', '"docs"', '"name":"blushy"', '"git":true', '"action":"accept"'])
      assert(txt.includes(want), `missing ${want} in ${txt}`);
  });

  await step("AskUserQuestion: preview, Other-only answer, empty multi-select", async () => {
    await sendPrompt(page, "quiz me");
    const card = page.locator("[data-card=question]");
    await card.waitFor();
    // Picking Redis shows its preview; Clear takes the pick back.
    await card.getByText("Redis", { exact: true }).click();
    await card.locator("[data-preview]").getByText("port: 6379").waitFor();
    await shot(page, "desktop-09-ask-user-question");
    await card.getByRole("button", { name: "Clear" }).click();
    assert((await card.locator("[data-preview]").count()) === 0, "preview still shown after Clear");
    // Answer with only typed text, leave the multi-select empty; Submit is enabled.
    await card.locator("input#q-question_0_custom").fill("Valkey 8");
    await card.getByRole("button", { name: "Submit" }).click();
    await waitIdle(page);
    const txt = await lastAgentText(page);
    assert(txt.includes('{"action":"accept","content":{"question_0_custom":"Valkey 8"}}'), txt);
  });

  await step("question form: Decline", async () => {
    await sendPrompt(page, "ask again");
    await page.locator("[data-card=question]").getByRole("button", { name: "Decline" }).click();
    await waitIdle(page);
    assert((await lastAgentText(page)).includes('"action":"decline"'), await lastAgentText(page));
  });

  await step("stop a running turn", async () => {
    await sendPrompt(page, "slow please");
    await page.locator("[data-status=responding]").waitFor();
    await page.waitForFunction(() => [...document.querySelectorAll(".md")].some((m) => m.textContent?.includes("tick 2")));
    await shot(page, "desktop-09-responding");
    await page.getByRole("button", { name: "stop" }).click();
    await waitIdle(page);
    await page.locator("[data-entry=turnEnd]").or(page.getByText("Interrupted")).last().waitFor();
    assert(mockLog().includes("cancelled"), "agent didn't see the cancel");
  });

  await step("queue a prompt while Claude responds", async () => {
    await sendPrompt(page, "slow again");
    await page.locator("[data-status=responding]").waitFor();
    await sendPrompt(page, "queued follow-up");
    await page.locator("[data-entry=user]").filter({ hasText: "queued follow-up" }).getByText("queued").waitFor();
    await page.locator("[data-queue]").waitFor();
    await shot(page, "desktop-10-queued");
    await page.getByRole("button", { name: "stop" }).click();
    // A stopped turn holds the queue: send it from the status row.
    await page.getByText("on hold").waitFor();
    await page.getByRole("button", { name: "send", exact: true }).click();
    await page.getByText("Done").last().waitFor({ timeout: 20_000 });
    await waitIdle(page);
  });

  await step("/web inside the terminal session reports the server", async () => {
    await sendPrompt(page, "/web");
    await page.locator("[data-entry=notice]").filter({ hasText: "blush web is running" }).waitFor({ timeout: 15_000 });
    await shot(page, "desktop-11-web-status-notice");
  });

  await step("jump-to-latest after scrolling up", async () => {
    await page.locator("[data-scroller]").evaluate((el) => el.scrollTo({ top: 0 }));
    await page.locator("[data-action=jump-latest]").click();
    await page.locator("[data-action=jump-latest]").waitFor({ state: "detached" });
  });

  await step("detach returns to the list", async () => {
    await page.locator("[data-action=detach]").click();
    await page.waitForFunction(() => !location.hash.startsWith("#/s/"));
    await page.getByText("Pick a session").waitFor();
  });

  await step("open a recent session (spawns a web host, replays history)", async () => {
    await page.locator("[data-history=mock-past01]").click();
    await page.locator("[data-entry=user]").filter({ hasText: "refactor the parser into modules" }).waitFor({ timeout: 30_000 });
    assert((await page.locator("[data-entry=user]").filter({ hasText: "refactor the parser" }).count()) === 1, "replayed history is duplicated");
    await page.locator("[data-host]").filter({ hasText: "Refactor the parser" }).waitFor();
    await page.locator("[data-menu=mode]").waitFor({ timeout: 30_000 });
    await sendPrompt(page, "continue the refactor");
    await waitIdle(page);
    await page.getByText("Done").last().waitFor();
    await shot(page, "desktop-12-web-session");
  });

  await step("switch between live sessions from the sidebar", async () => {
    await page.locator("[data-host]").filter({ hasText: "terminal" }).click();
    await page.locator("[data-entry=user]").filter({ hasText: "add a div" }).waitFor();
    await page.locator("[data-host]").filter({ hasText: "Refactor the parser" }).click();
    await page.locator("[data-entry=user]").filter({ hasText: "continue the refactor" }).waitFor();
  });

  await step("end a web session (confirm dialog)", async () => {
    await page.locator("[data-action=session-menu]").click();
    await page.getByRole("menuitem", { name: "End session" }).click();
    await page.getByRole("alertdialog").waitFor();
    await shot(page, "desktop-13-end-confirm");
    await page.locator("[data-action=confirm-end]").click();
    await page.getByText("The session ended").or(page.getByText("Back to sessions")).first().waitFor({ timeout: 15_000 });
    await page.getByRole("button", { name: "Back to sessions" }).click();
    await page.waitForFunction(() => document.querySelectorAll("[data-host]").length === 1, null, { timeout: 15_000 });
  });

  await step("new session dialog: bad folder shows an error", async () => {
    await page.locator("[data-action=new-session]").click();
    await page.getByRole("dialog").waitFor();
    await page.locator("#new-cwd").fill(join(TMP, "does-not-exist"));
    await page.locator("[data-action=create-session]").click();
    await page.getByText("No such directory").waitFor();
  });

  await step("new session dialog: start a session in a folder", async () => {
    await page.getByRole("dialog").getByRole("button", { name: /proj/ }).first().click();
    assert((await page.locator("#new-cwd").inputValue()).endsWith("proj"), await page.locator("#new-cwd").inputValue());
    await shot(page, "desktop-14-new-session");
    await page.locator("[data-action=create-session]").click();
    await page.getByRole("dialog").waitFor({ state: "detached" });
    await page.locator("[data-menu=mode]").waitFor({ timeout: 30_000 });
    await sendPrompt(page, "hello from the web");
    await waitIdle(page);
    await page.getByText("Done").last().waitFor();
  });

  await step("theme menu switches the palette (and back)", async () => {
    await page.locator("[data-action=session-menu]").click();
    await page.getByRole("menuitem").filter({ hasText: "rosepine" }).click();
    const accent = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--accent"));
    assert(accent.trim() && accent.trim() !== "#f2b8cf", `accent ${accent}`);
    await shot(page, "desktop-15-theme-rosepine");
    await page.locator("[data-action=theme]").click();
    await page.getByRole("menuitem").filter({ hasText: /^blush\(config\)/ }).click();
    const back = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--accent"));
    assert(back.trim() === "#f2b8cf", `accent ${back}`);
  });

  await step("copy resume command", async () => {
    await desk.grantPermissions(["clipboard-read", "clipboard-write"], { origin: base });
    await page.locator("[data-action=session-menu]").click();
    await page.getByRole("menuitem", { name: "Copy resume command" }).click();
    await page.getByText("Copied the resume command").waitFor();
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    assert(clip.startsWith("blush --resume mock-"), clip);
  });

  await step("refresh Recent", async () => {
    await page.locator("[data-action=refresh-history]").click();
    await page.locator("[data-history]").first().waitFor();
  });

  // ------------------------------------------------ Pixel 8
  console.log("Pixel 8 (412×915 @2.625)");
  const phone = await browser.newContext({
    viewport: { width: 412, height: 915 },
    deviceScaleFactor: 2.625,
    isMobile: true,
    hasTouch: true,
    userAgent: "Mozilla/5.0 (Linux; Android 15; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
  });
  const mp = await phone.newPage();
  current = mp;
  mp.on("pageerror", (e) => consoleErrors.push(String(e)));

  await step("phone: the printed LAN link unlocks and redirects", async () => {
    await mp.goto(lanUrl);
    await mp.waitForURL((u) => !u.search.includes("token"));
    await mp.locator("[data-host]").first().waitFor();
    await shot(mp, "pixel8-01-list");
  });

  await step("phone: attach shows only the session, with a back button", async () => {
    await mp.locator("[data-host]").filter({ hasText: "terminal" }).tap();
    await mp.locator("textarea[aria-label=Prompt]").waitFor();
    assert(!(await mp.locator("[data-history]").first().isVisible()), "list still visible");
    await mp.locator("[data-menu=mode]").waitFor();
    await shot(mp, "pixel8-02-session");
  });

  await step("phone: Enter adds a newline, the button sends", async () => {
    const box = mp.locator("textarea[aria-label=Prompt]");
    await box.tap();
    await box.pressSequentially("from my phone");
    await box.press("Enter");
    assert((await box.inputValue()) === "from my phone\n", JSON.stringify(await box.inputValue()));
    await mp.locator("[data-send]").tap();
    await mp.locator("[data-entry=user]").filter({ hasText: "from my phone" }).waitFor();
    await waitIdle(mp);
    await mp.getByText("Done").last().waitFor();
    await shot(mp, "pixel8-03-turn");
  });

  await step("phone: permission card", async () => {
    await sendPrompt(mp, "perm from phone");
    await mp.locator("[data-card=permission]").waitFor();
    await shot(mp, "pixel8-04-permission");
    await mp.locator("[data-card=permission] [data-option=allow_always]").tap();
    await waitIdle(mp);
    assert(mockLog().includes("permission -> always"), "agent didn't get always");
  });

  await step("phone: question card", async () => {
    await sendPrompt(mp, "ask from phone");
    const card = mp.locator("[data-card=question]");
    await card.waitFor();
    await card.getByText("TypeScript").tap();
    await card.getByRole("checkbox", { name: "CI" }).tap();
    await shot(mp, "pixel8-05-question");
    await card.getByRole("button", { name: "Submit" }).tap();
    await waitIdle(mp);
    assert((await lastAgentText(mp)).includes('"lang":"ts"'), await lastAgentText(mp));
  });

  await step("phone: mode and model menus", async () => {
    await mp.locator("[data-menu=mode]").tap();
    await mp.getByRole("menuitem").first().waitFor();
    await shot(mp, "pixel8-06-mode-menu");
    await mp.getByRole("menuitem").filter({ hasText: "always-approve" }).tap();
    await mp.waitForFunction(() => document.querySelector("[data-menu=mode]")?.textContent?.includes("always-approve"));
    await mp.locator("[data-menu=model]").tap();
    await mp.getByRole("menuitem").filter({ hasText: "Haiku" }).first().tap();
    await mp.waitForFunction(() => document.querySelector("[data-menu=model]")?.textContent?.includes("Haiku"));
  });

  await step("phone: stop button", async () => {
    await sendPrompt(mp, "slow phone");
    await mp.getByText("tick 1", { exact: true }).waitFor();
    await shot(mp, "pixel8-07-responding");
    await mp.getByRole("button", { name: "stop" }).tap();
    await waitIdle(mp);
  });

  await step("phone: session menu", async () => {
    await mp.locator("[data-action=session-menu]").tap();
    await mp.getByRole("menuitem", { name: "Detach" }).waitFor();
    await shot(mp, "pixel8-08-session-menu");
    await mp.keyboard.press("Escape");
  });

  await step("phone: back button detaches", async () => {
    await mp.locator("[data-action=back]").tap();
    await mp.locator("[data-action=new-session]").waitFor();
    assert(!(await mp.locator("textarea[aria-label=Prompt]").isVisible()), "session still visible");
  });

  await step("phone: new session dialog", async () => {
    await mp.locator("[data-action=new-session]").tap();
    await mp.getByRole("dialog").waitFor();
    await shot(mp, "pixel8-09-new-session");
    await mp.getByRole("button", { name: "Cancel" }).tap();
    await mp.getByRole("dialog").waitFor({ state: "detached" });
  });

  await step("phone: open a recent session, then use the browser's back", async () => {
    await mp.locator("[data-history]").first().tap();
    await mp.locator("textarea[aria-label=Prompt]").waitFor({ timeout: 30_000 });
    await mp.locator("[data-menu=mode]").waitFor({ timeout: 30_000 });
    await shot(mp, "pixel8-10-recent-session");
    await mp.goBack();
    await mp.locator("[data-action=new-session]").waitFor();
    assert(!(await mp.locator("textarea[aria-label=Prompt]").isVisible()), "session still visible after back");
  });

  await step("phone: theme menu", async () => {
    await mp.locator("[data-action=theme]").tap();
    await mp.getByRole("menuitem").filter({ hasText: "night" }).tap();
    await shot(mp, "pixel8-11-theme-night");
    await mp.locator("[data-action=theme]").tap();
    await mp.getByRole("menuitem").filter({ hasText: /^blush\(config\)/ }).tap();
  });

  await step("desktop sees the phone's prompts live (same session)", async () => {
    current = page;
    await page.locator("[data-host]").filter({ hasText: "terminal" }).click();
    await page.locator("[data-entry=user]").filter({ hasText: "from my phone" }).waitFor();
  });

  await step("reconnects after the server restarts", async () => {
    current = mp;
    const down = blush("web", "down");
    assert(down.status === 0 && /stopped/.test(down.stdout), down.stdout + down.stderr);
    await mp.getByText("reconnecting").waitFor({ timeout: 10_000 });
    await shot(mp, "pixel8-12-reconnecting");
    const up = blush("web", "up");
    assert(up.status === 0, up.stderr);
    await mp.getByText("reconnecting").waitFor({ state: "detached", timeout: 20_000 });
    // The terminal session links back up on its own.
    await mp.locator("[data-host]").filter({ hasText: "terminal" }).waitFor({ timeout: 15_000 });
  });

  await step("lock this browser", async () => {
    await mp.locator("[data-action=lock-menu]").tap();
    await mp.getByRole("menuitem", { name: "Lock this browser" }).tap();
    await mp.getByRole("form", { name: "Unlock" }).waitFor();
    await shot(mp, "pixel8-13-locked");
  });

  await step("no page errors", async () => {
    const real = consoleErrors.filter((e) => !/WebSocket|ERR_CONNECTION|Failed to load resource/.test(e));
    assert(real.length === 0, real.join("\n"));
  });

  await browser.close();

  await step("cli: `blush web down` stops the server", async () => {
    const r = blush("web", "down");
    assert(/stopped/.test(r.stdout), r.stdout);
    const s = blush("web");
    assert(/isn't running/.test(s.stdout), s.stdout);
    const again = blush("web", "down");
    assert(/wasn't running/.test(again.stdout), again.stdout);
  });

  stopTui();
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  for (const f of failed) console.log(`  ✗ ${f.name}: ${f.err}`);
  writeFileSync(join(OUT, "results.json"), JSON.stringify(results, null, 2));
  process.exit(failed.length ? 1 : 0);
}

main().catch((e) => {
  console.error(e);
  stopTui();
  blush("web", "down");
  process.exit(1);
});
