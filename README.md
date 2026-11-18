<div align="center">

# blush

**A full-screen, mouse-driven terminal UI for Claude Code.**

Grok Build's look and feel, with Claude Code doing the work underneath, connected over the
[Agent Client Protocol](https://agentclientprotocol.com).

[![Rust 2024](https://img.shields.io/badge/rust-1.85%2B%20·%20edition%202024-f2b8cf?style=flat-square&logo=rust&logoColor=white&labelColor=1b1b1f)](https://www.rust-lang.org)
[![Node 20+](https://img.shields.io/badge/node-20%2B-f2b8cf?style=flat-square&logo=nodedotjs&logoColor=white&labelColor=1b1b1f)](https://nodejs.org)
[![Platforms](https://img.shields.io/badge/platforms-windows%20·%20macos%20·%20linux-f2b8cf?style=flat-square&labelColor=1b1b1f)](#install)
[![License](https://img.shields.io/badge/license-Apache--2.0-f2b8cf?style=flat-square&labelColor=1b1b1f)](#license)

[Install](#install) · [Usage](#usage) · [Keys](#keyboard--mouse) · [Commands](#slash-commands) · [Power tools](#power-tools) · [Themes](#themes) · [Config](#configuration) · [How it works](#how-it-works)

</div>

```
~/projects/app                                                          31.0K / 1.0M

▌ ❯ add a div function that raises on zero                                  7:53 PM

  ◆ Read calc.py
  ◆ Write calc.py +9 -1  1.6s
     10  def div(a, b):
     11      """Return a divided by b; raise ZeroDivisionError if b is zero."""
  ◆ Thought for 2.0s

  I added div to calc.py …                                                  7:53 PM

⁙ Responding… 4s                                                  4s ⇣36 [stop]
╭──────────────────────────────────────────────────────────────────────────────╮
│ ❯                                                                            │
╰─────────────────────────────────────── Claude Opus 5.5 (high) · always-approve ╯
 Shift+Tab:mode  │  Ctrl+c:cancel  │  Ctrl+.:shortcuts
```

## Why blush?

- **Claude Code underneath.** blush talks ACP to
  [`@agentclientprotocol/claude-agent-acp`](https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp),
  which runs Claude Code through the Claude Agent SDK. You keep your models, your login,
  your `CLAUDE.md`, and your custom slash commands.
- **Built on Grok Build's stack:** Rust, ratatui, crossterm, tokio, syntect, pulldown-cmark, and similar.
  It's a single native binary that starts instantly.
- **A transcript you can browse.** You can fold any tool call, open it in a viewer, copy it,
  and jump from one turn to the next. Diffs show inline with line gutters, and code blocks
  are syntax-highlighted.
- **The mouse works.** Scroll with the wheel, click a block to select it, and click its header to fold it.
- **Gets out of your way.** Sessions start in `bypassPermissions` mode (shown as
  `always-approve`). Press `Shift+Tab` to switch to a stricter mode whenever you like.
- **Power tools built in.** A Time Machine that snapshots your files after every turn, `/commit`
  into Conventional Commits, meters for your subscription, context, and tokens, a spoken summary
  of each turn, and one-character prefixes that run a single turn on Haiku or at max effort.
- **Headless mode.** `blush -p "…"` streams the answer to stdout, so you can use it in scripts and pipes.
- **Five themes.** The default is a grey `#1b1b1f` canvas with a soft pink `#f2b8cf` accent.
- **Cross-platform.** It runs on Windows, macOS, and Linux. On Windows it launches the adapter
  with `node` directly, without going through `.cmd` shims.

## Requirements

| | |
|---|---|
| **Rust 1.85+** | Edition 2024. Only needed to build. |
| **Node.js 20+** | `node` and `npm` must be on your PATH. On first launch blush installs the ACP adapter into `~/.blush/adapter`. |
| **A Claude Code account** | If you aren't signed in, run `/login` inside blush, or `claude auth login` in a shell. |

## Install

```sh
git clone https://github.com/thePanda-X/blush.git
cd blush
make          # detects your OS, checks prerequisites, builds a release binary, installs it
blush
```

`make` installs into `~/.cargo/bin`, which rustup already adds to your PATH.

<details>
<summary><b>Platform notes and other make targets</b></summary>

<br>

- **Windows:** you need GNU Make (`choco install make` or `winget install GnuWin32.Make`).
  The recipes run in PowerShell. If the install folder isn't on your user PATH, `make` adds it.
- **macOS:** `make` clears the quarantine flag and ad-hoc signs the binary, so Gatekeeper
  doesn't kill it.
- **Without make:** `cargo install --path .`

| Target | What it does |
|---|---|
| `make` | Build and install (the default) |
| `make build` | Release build only |
| `make install BINDIR=/usr/local/bin` | Install somewhere else |
| `make uninstall` | Remove the installed binary |
| `make adapter` | Install or update the Claude Code ACP adapter ahead of time |
| `make check` | Check that cargo, node, npm, and claude are available |
| `make test` | Run the unit tests |
| `make clean` | `cargo clean` |

</details>

## Usage

```sh
blush                              # interactive TUI in the current directory
blush "fix the failing test"       # start with a prompt
blush -c                           # continue the most recent session here
blush -r                           # pick a session to resume
blush -r <session-id>              # resume a specific session
blush -p "summarize README.md"     # headless: stream the answer to stdout and exit
blush --cwd ../other-project       # run the agent in another directory
blush --mode plan --model sonnet --theme night
blush --no-mouse                   # keep your terminal's native selection
blush --update-adapter             # reinstall the latest Claude Code ACP adapter
```

When you quit, blush prints a `blush --resume <id>` line so you can return to the session later.

In headless mode (`-p`), the agent's text goes to **stdout** and tool activity goes to **stderr**.
That means `blush -p "…" > answer.md` saves only the answer.

## Keyboard & mouse

| Key | Action |
|---|---|
| `Enter` | Send. If Claude is still responding, the prompt is queued. |
| `Shift+Enter` · `Alt+Enter` · `Ctrl+J` · `\` + `Enter` | New line |
| `Shift+Tab` | Cycle permission mode: always-approve → ask → accept-edits → plan → auto |
| `Ctrl+C` | First press stops the response, the next clears the prompt, and pressing twice on an empty prompt quits |
| `Tab` / `↑` on an empty prompt | Move focus to the scrollback |
| `↑` `↓` · `←` `→` · `Enter` · `y` | In the scrollback: select an entry, fold or unfold it, open it in the viewer, copy it |
| `o` / `r` | In the scrollback: open the selected tool's file, or reveal it in Explorer / Finder |
| `Shift+←` / `Shift+→` | Jump between turns |
| `Ctrl+E` / `Ctrl+O` | Toggle thinking blocks / toggle all blocks (and, in compact mode, all groups) |
| `Ctrl+G` | Toggle the compact layout |
| `PgUp` / `PgDn`, mouse wheel | Scroll |
| Click | Click a block to select it; click its header to fold it |
| `Ctrl+.` / `F1` | Show all shortcuts |

> **Tip:** Hold **Shift** while dragging to use your terminal's own text selection.
> If you'd rather turn mouse capture off entirely, pass `--no-mouse` or set `ui.mouse = false`.

## Slash commands

| Command | Description |
|---|---|
| `/new` (`/clear`) | Start a fresh session |
| `/resume` | Pick a previous session |
| `/model` · `/effort` · `/fast` | Change the model, reasoning effort, or fast mode |
| `/mode` | Choose a permission mode |
| `/always-approve` (`/yolo`) · `/ask` · `/accept-edits` · `/plan` | Switch directly to a permission mode |
| `/theme` (`/t`) | Change the theme |
| `/copy` · `/export` | Copy the last answer, or export the transcript as Markdown |
| `/context` · `/session-info` | Show context usage and cost, or session, model, and adapter details |
| `/thinking` · `/timestamps` | Toggle thinking blocks or timestamps |
| `/restart` | Restart the agent process |
| `/login` | Run `claude auth login` without leaving blush |
| `/config` | Show where the config file is |
| `/shortcuts` (`/help`, `/keys`) | Show the shortcut overlay |
| `/commit` | Commit your changes as Conventional Commits, on Haiku |
| `/timeline` · `/undo` · `/redo` | Browse and restore Time Machine snapshots |
| `/after <task>` | Queue a task to run once Claude finishes |
| `/narrator` | Turn spoken turn summaries on or off |
| `/compact-mode` | Toggle the compact layout (same as `Ctrl+G`) |
| `/quit` (`/exit`, `/q`) | Quit |

Every command Claude Code advertises works too, including `/compact`, `/review`, and your own
custom commands.

Type **`@path`** to fuzzy-search files in the project. The file you pick is attached to your prompt
as a resource link.

## Power tools

### Compact layout

Press **`Ctrl+G`** (or run `/compact-mode`) to fold each run of tool calls and thinking blocks into
a single line between Claude's replies:

```
  ❯ add a div function that raises on zero

  ◆ 2 tools · 1 thought · 1 edit +9 -1  6s

  I added div to calc.py …
```

While Claude works, the line pulses and shows the current step (`Running cargo test`, `Thinking…`).
To open just one group, select it and press `→` or `Enter`, or click it. A `▾` header line appears
above the group. Click that header, or press `←` on any entry in the group, to fold it back.
`Ctrl+O` opens or closes every group at once, and `y` or `f` on a folded group copies or views
everything inside it.
Only runs of two or more blocks are grouped, because a single tool call already takes one line.
The setting is saved as `ui.compact`.

### Per-turn model and effort

Start a prompt with **`_`** to run just that turn on Haiku, or with **`+`** to run it at max effort.
You can combine them (`_+`). The override shows on the prompt, on the spinner, and next to the
turn's stats, and the session's own model and effort come back as soon as the turn ends.

```
_ rename these variables to snake_case
+ find the race condition in the scheduler
```

`__init__.py …` and `+1 …` are left alone, so prompts that happen to start with those characters still work.

### Time Machine

blush takes a git snapshot of your working tree before the first turn and after every turn that
changes files. That includes changes made through Bash, not just through edit tools. Snapshots go
into a private index (`.git/blush-timeline.index`) and `refs/blush/timeline/*`, so your staging
area, `HEAD`, and branches are never touched.

- `/timeline` lists every state: `◉` is where you are, `●` is earlier, and `○` is undone.
  Press `→` to preview a snapshot's diff and `Enter` to restore it.
- `/undo` and `/redo` step back and forth one snapshot at a time.
- Before any jump, blush snapshots your current files, so you never lose edits you haven't saved.

The Time Machine restores files, not the conversation: Claude still remembers the turns you undid.
It needs the working directory to be inside a git repository.

### `/commit`

`/commit` asks how to finish: **commit and push** (recommended), **commit only**, or **commit on a
new branch**. Then a Haiku turn reads your diff and splits it into one Conventional Commit per
logical change on your branch. It won't stage secrets such as `.env` files or keys, and it adds no
AI attribution. You can skip the menu with `/commit push`, `/commit commit`, or `/commit branch`.

### Meters

| Meter | Where | What it shows |
|---|---|---|
| **Context** | Above the prompt, right | How full the context window is. Dim below 50%, yellow from 50% (a good time to `/compact`), red from 75% (consider `/new`). |
| **Tokens** | Above the prompt, right | The last turn's input, output, and cache reads and writes, plus running totals for the session. |
| **Subscription** | Footer, right | Your 5-hour and weekly limits, alternating every few seconds, with a bar, the percentage used, and the time until each resets. |

The subscription meter comes from Claude Code's `/usage` command. That command runs locally and
doesn't call the model. blush runs it at startup and then at most every five minutes, in a
throwaway background session that it closes and deletes right away. It never touches your own
session or history.

### Narrator

`/narrator`, or a click on the `⏸ narrator off` toggle in the footer, turns on spoken summaries.
After each turn, blush reads the opening of Claude's final message aloud with your system voice:
System.Speech on Windows, `say` on macOS, and `spd-say` or `espeak-ng` on Linux. The toggle shows
`⏹ narrating` while it speaks, and clicking it stops the speech. Sending a new prompt also stops it.

### Quick Peek

Select a tool entry in the scrollback (click it, or `Tab` and then `↑`/`↓`). The footer shows the
file's name, type, size, and how long ago it was modified. Press `o` to open the file in its
default app, or `r` to reveal it in Explorer / Finder / your file manager.

### `/after`

`/after <task>` queues a task to run when the current turn finishes, without interrupting it.
Pressing `Enter` while Claude is responding does the same. `/after` just makes your intent explicit,
and it accepts the `_` and `+` prefixes.

## Themes

| Theme | Description |
|---|---|
| `blush` | A grey canvas with a light pink accent. This is the default. |
| `night` | Neutral near-black with a violet accent |
| `rosepine` | A muted palette inspired by Rosé Pine Moon |
| `day` | A light theme for bright terminals |
| `terminal` | Uses your terminal's own colors, with no background fills |

Switch with `/theme`, `--theme <name>`, or `ui.theme` in the config.
You can also override any theme's colors with `ui.accent` and `ui.background`.

## Configuration

blush creates `~/.blush/config.toml` the first time it runs. Set `BLUSH_HOME` to keep it somewhere else.

```toml
[ui]
theme = "blush"          # blush | night | rosepine | day | terminal
accent = "#f2b8cf"       # optional override
background = "#1b1b1f"   # optional override
show_timestamps = true
mouse = true
show_thinking = true
expand_thinking = false
animations = true
compact = false          # group tool calls & thinking between replies (Ctrl+G)

[agent]
default_mode = "bypassPermissions"   # default | acceptEdits | plan | auto | bypassPermissions
model = ""                           # e.g. "opus", "sonnet", or a full model id
effort = ""                          # low | medium | high | xhigh | max
use_system_claude = false            # drive the `claude` on PATH instead of the SDK's bundled CLI
command = ""                         # run any other ACP agent instead
args = []

[agent.env]                          # extra env for the agent process
# CLAUDE_CODE_THINKING_DISPLAY = "omitted"

[features]
narrator = false      # spoken turn summaries (toggle with /narrator)
sub_meter = true      # 5-hour / weekly subscription meter
token_meter = true    # per-turn and session token counts
timeline = true       # Time Machine snapshots after each turn
```

> **Bring your own agent.** blush speaks plain ACP, so setting `agent.command` and `agent.args` lets
> you point it at any ACP-compatible agent instead of Claude Code.

### Files

| Path | Contents |
|---|---|
| `~/.blush/config.toml` | Settings |
| `~/.blush/history` | Prompt history |
| `~/.blush/logs/agent.log` | The adapter's stderr, which is the first place to look when something goes wrong |
| `~/.blush/adapter/` | The npm install of the ACP adapter that blush manages |

### About bypass mode

Claude Code refuses bypass mode for root and sudo users unless `IS_SANDBOX=1` is set. Managed
settings can also disable it. In either case blush shows a warning and starts in the agent's
default mode instead.

## How it works

```
┌──────────────────┐   JSON-RPC over stdio (ACP)   ┌──────────────────────┐        ┌──────────────────────┐
│ blush            │ ────────────────────────────▶ │ claude-agent-acp     │ ─────▶ │ Claude Code          │
│ ratatui TUI      │ ◀──────────────────────────── │ node                 │        │ Claude Agent SDK     │
└──────────────────┘   session/update, permission  └──────────────────────┘        └──────────────────────┘
```

| Module | Responsibility |
|---|---|
| [`src/acp.rs`](src/acp.rs) | A small ACP client that accepts loosely formed input (newline-delimited JSON-RPC 2.0). It keeps messages as untyped JSON, so the adapter's many extensions never break it. |
| [`src/launcher.rs`](src/launcher.rs) | Finds or installs the adapter, runs it with `node` directly, and kills the whole process tree on exit. |
| [`src/model.rs`](src/model.rs) | The transcript model and the `session/update` reducer, which handles message and thought chunks, tool calls, plans, modes, config options, usage, and titles. |
| [`src/app.rs`](src/app.rs) | App state, keyboard and mouse handling, slash commands, and the permission and question cards (`session/request_permission`, `elicitation/create`). |
| [`src/ui.rs`](src/ui.rs), [`src/blocks.rs`](src/blocks.rs) | Grok-style rendering: the accent rail, the ◆ bullets that pulse while a tool runs, the user-prompt band, inline diffs, the status row, and the rounded prompt box. |
| [`src/markdown.rs`](src/markdown.rs) | Renders markdown for ratatui, with syntect highlighting and tables. |
| [`src/headless.rs`](src/headless.rs) | The one-shot `-p` mode. |
| [`src/input.rs`](src/input.rs), [`src/files.rs`](src/files.rs) | The prompt editor and `@path` fuzzy file completion. |
| [`src/theme.rs`](src/theme.rs), [`src/config.rs`](src/config.rs) | Themes, colour overrides, and `config.toml`. |
| [`src/timeline.rs`](src/timeline.rs) | The Time Machine: snapshots in a private git index, and restores that only touch the paths that changed. |
| [`src/usage.rs`](src/usage.rs) | Parses `/usage` into the subscription meter, and reads per-turn token usage from prompt responses. |
| [`src/narrator.rs`](src/narrator.rs), [`src/peek.rs`](src/peek.rs) | Text-to-speech summaries, and opening or revealing files. |

## Troubleshooting

- **Sign-in or auth errors:** run `/login` inside blush, or `claude auth login` in a shell.
- **The adapter fails to start:** check `~/.blush/logs/agent.log`, then try `blush --update-adapter`.
- **`blush` isn't found after installing:** open a new terminal. On macOS and Linux, also check
  that `~/.cargo/bin` is on your `PATH`.
- **Text selection doesn't work:** hold **Shift** while dragging, or run with `--no-mouse`.
- **To see raw protocol data:** set `BLUSH_DEBUG=1` in headless mode to print the `session/new` response.

## Development

```sh
cargo run -- "hello"     # debug build
cargo test               # unit tests
make                     # release build and install
```

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)

<sub>blush is an independent project. It is not affiliated with Anthropic or xAI. "Claude" and
"Claude Code" are trademarks of Anthropic.</sub>
