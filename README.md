# blush

A full-screen, mouse-interactive terminal coding agent with the look and feel
of [Grok Build](https://github.com/xai-org/grok-build), driving **Claude Code**
under the hood over the [Agent Client Protocol](https://agentclientprotocol.com).

- **Same stack as Grok Build:** Rust · ratatui · crossterm · tokio · syntect · pulldown-cmark · similar
- **Claude Code backend:** talks ACP to [`@agentclientprotocol/claude-agent-acp`](https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp),
  which runs Claude Code through the Claude Agent SDK
- **Bypass permissions by default:** every session starts in `bypassPermissions` (shown as `always-approve`)
- **Dark theme by default:** grey canvas `#1b1b1f`, light pink accent `#f2b8cf`
- **Windows and macOS** (Linux works too)

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

## Requirements

- **Rust** 1.85+ (edition 2024), to build
- **Node.js 20+** (`node` and `npm` on PATH). On first launch blush installs the ACP
  adapter into `~/.blush/adapter`.
- **A signed-in Claude Code** account. If you're not signed in, run `/login` inside blush
  (it runs `claude auth login`) or `claude auth login` in a shell.

## Install

```sh
cargo install --path .
blush
```

Or build it with `cargo build --release`; the binary is `target/release/blush` (`blush.exe` on Windows).

## Usage

```sh
blush                      # interactive TUI in the current directory
blush "fix the failing test"   # start with a prompt
blush -c                   # continue the most recent session here
blush -r                   # pick a session to resume
blush -r <session-id>      # resume a specific session
blush -p "summarize README.md"  # headless: stream the answer to stdout and exit
blush --mode plan --model sonnet --theme night
blush --update-adapter     # reinstall the latest Claude Code ACP adapter
```

### Keys

| Key | Action |
|---|---|
| `Enter` | Send (queues the prompt while Claude is responding) |
| `Shift+Enter` · `Alt+Enter` · `Ctrl+J` · `\`+`Enter` | New line |
| `Shift+Tab` | Cycle permission mode (always-approve → ask → accept-edits → plan → auto) |
| `Ctrl+C` | Stop the response, then clear the prompt, then quit (press twice) |
| `Tab` / `↑` on an empty prompt | Focus the scrollback |
| `↑↓` · `←→` · `Enter` · `y` | Select entry · fold/unfold · open viewer · copy (scrollback) |
| `Shift+←/→` | Jump between turns |
| `Ctrl+E` / `Ctrl+O` | Toggle thinking / all blocks |
| `PgUp/PgDn`, mouse wheel, click | Scroll; click a block to select it, click its header to fold it |
| `Ctrl+.` / `F1` | Shortcuts |

Hold **Shift** while dragging to use your terminal's native text selection
(mouse capture is on by default; pass `--no-mouse` or set `ui.mouse = false` to turn it off).

### Slash commands

`/new` `/resume` `/model` `/effort` `/mode` `/always-approve` `/ask` `/accept-edits`
`/plan` `/fast` `/theme` `/copy` `/export` `/context` `/session-info` `/thinking`
`/timestamps` `/restart` `/login` `/config` `/shortcuts` `/quit`, plus every command
Claude Code advertises (`/compact`, `/review`, your custom commands, …).
`@path` mentions fuzzy-complete files and attach them as resource links.

## Configuration

`~/.blush/config.toml` is created on first run. Set `BLUSH_HOME` to move it.

```toml
[ui]
theme = "blush"          # blush | night | rosepine | day | terminal
accent = "#f2b8cf"       # optional override
background = "#1b1b1f"   # optional override
show_timestamps = true
mouse = true
show_thinking = true
expand_thinking = false

[agent]
default_mode = "bypassPermissions"   # default | acceptEdits | plan | auto | bypassPermissions
model = ""                           # e.g. "opus", "sonnet", or a full model id
effort = ""                          # low | medium | high | xhigh | max
use_system_claude = false            # drive the `claude` on PATH instead of the SDK's bundled CLI
command = ""                         # run any other ACP agent instead
args = []

[agent.env]                          # extra env for the agent process
# CLAUDE_CODE_THINKING_DISPLAY = "omitted"
```

Claude Code won't allow bypass mode for root/sudo users unless `IS_SANDBOX=1` is set,
or when managed settings disable it. In those cases blush shows a warning and stays in the
agent's default mode.

Files under `~/.blush/`: `config.toml`, `history` (prompt history), `logs/agent.log`
(the adapter's stderr), and `adapter/` (the managed npm install).

## How it works

```
blush (ratatui TUI) ──JSON-RPC over stdio (ACP)──▶ claude-agent-acp (node) ──▶ Claude Code (Agent SDK)
```

- `src/acp.rs`: a small, tolerant ACP client (newline-delimited JSON-RPC 2.0).
  It keeps messages as untyped JSON, so the adapter's many extensions never break it.
- `src/launcher.rs`: finds or installs the adapter. It runs the adapter with `node` directly,
  so Windows `.cmd` shims are never involved, and it kills the whole process tree on exit.
- `src/model.rs`: the transcript model and the `session/update` reducer (message and thought chunks,
  tool calls, plans, modes, config options, usage, titles).
- `src/app.rs`: state, keyboard and mouse handling, slash commands, permission and question cards
  (`session/request_permission`, `elicitation/create`).
- `src/ui.rs` and `src/blocks.rs`: Grok-style rendering. That means the accent rail, ◆ bullets that
  pulse while running, the user-prompt band, inline diffs with line gutters, the status row,
  and the rounded prompt box.
- `src/markdown.rs`: markdown → ratatui, with syntect highlighting and tables.

Run the tests with `cargo test`.
