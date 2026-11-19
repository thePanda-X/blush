mod acp;
mod app;
mod attach;
mod blocks;
mod config;
mod files;
mod headless;
mod input;
mod launcher;
mod markdown;
mod model;
mod narrator;
mod notify;
mod peek;
mod shell;
mod text;
mod theme;
mod timeline;
mod ui;
mod update;
mod usage;
mod web;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, EventStream,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen, SetTitle};
use crossterm::{cursor, execute};
use futures::{FutureExt, StreamExt};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use app::{App, AppMsg, External, SessionKind};
use config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "blush",
    version = env!("BLUSH_VERSION"),
    about = "A Grok-Build-style coding agent TUI for Claude Code (via ACP)",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Initial prompt to send once the session is ready.
    prompt: Option<String>,

    /// Run headless: send PROMPT, stream the answer to stdout, then exit.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    print: Option<String>,

    /// Continue the most recent session in this directory.
    #[arg(short = 'c', long = "continue")]
    continue_latest: bool,

    /// Resume a session by id (or pick one when no id is given).
    #[arg(short = 'r', long = "resume", value_name = "SESSION_ID", num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,

    /// Working directory for the agent.
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// Theme: blush | blush-light | night | rosepine | day | terminal
    #[arg(long)]
    theme: Option<String>,

    /// Permission mode to start in (default: bypassPermissions).
    #[arg(long, value_name = "MODE")]
    mode: Option<String>,

    /// Model to select at startup (e.g. opus, sonnet, or a full id).
    #[arg(long)]
    model: Option<String>,

    /// Disable mouse capture (keeps native terminal selection).
    #[arg(long)]
    no_mouse: bool,

    /// Reinstall the Claude Code ACP adapter at its latest version and exit.
    #[arg(long)]
    update_adapter: bool,

    /// Update blush to the latest commit on GitHub and exit.
    #[arg(long)]
    update: bool,

    /// Skip the launch-time update check for this run.
    #[arg(long)]
    no_update: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// The web companion: attach to sessions from a browser or your phone.
    Web {
        #[command(subcommand)]
        action: Option<WebAction>,
    },
}

#[derive(clap::Subcommand, Debug)]
enum WebAction {
    /// Show whether the web server is running and where to open it (the default).
    Status,
    /// Start the web server in the background.
    Up,
    /// Stop the web server (and the sessions it opened).
    Down,
    /// Run the web server in the foreground.
    #[command(hide = true)]
    Serve,
    /// Run one session headless, driven from the web (started by the server).
    #[command(hide = true)]
    Host {
        #[arg(long)]
        id: String,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        resume: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load();
    if let Some(Command::Web { action }) = cli.command {
        return run_web(action, config);
    }
    if cli.update {
        std::process::exit(update::run_now(&config.update));
    }
    // Interactive launches only: never stall scripts (-p) with a rebuild.
    if cli.print.is_none()
        && !cli.update_adapter
        && !cli.no_update
        && let Some(code) = update::on_launch(&config.update)
    {
        std::process::exit(code);
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(run(cli, config))
}

async fn run(cli: Cli, mut config: Config) -> Result<()> {
    if let Some(t) = &cli.theme {
        if theme::Theme::by_name(t).is_none() {
            let names: Vec<&str> = theme::THEME_NAMES.iter().map(|(n, _)| *n).collect();
            anyhow::bail!("unknown theme `{t}` (choose from: {})", names.join(", "));
        }
        config.ui.theme = t.clone();
    }
    if let Some(m) = &cli.mode {
        config.agent.default_mode = m.clone();
    }
    if let Some(m) = &cli.model {
        config.agent.model = m.clone();
    }
    if cli.no_mouse {
        config.ui.mouse = false;
    }
    let cwd = match &cli.cwd {
        Some(p) if !p.is_dir() => anyhow::bail!("--cwd {}: no such directory", p.display()),
        Some(p) => dunce_canon(p),
        None => std::env::current_dir()?,
    };

    if cli.update_adapter {
        launcher::install_adapter(|m| eprintln!("{m}")).await?;
        return Ok(());
    }
    if let Some(prompt) = cli.print {
        let code = headless::run(config, cwd, prompt).await?;
        std::process::exit(code);
    }

    markdown::preload();
    let startup = match &cli.resume {
        Some(id) if !id.is_empty() => SessionKind::Load(id.clone()),
        _ => SessionKind::New,
    };
    let pick_session = matches!(&cli.resume, Some(id) if id.is_empty());
    run_tui(config, cwd, startup, cli.prompt, cli.continue_latest, pick_session).await
}

fn run_web(action: Option<WebAction>, config: Config) -> Result<()> {
    let rt = || tokio::runtime::Builder::new_multi_thread().enable_all().build();
    match action {
        None | Some(WebAction::Status) => std::process::exit(web::cli(Some("status"))),
        Some(WebAction::Up) => std::process::exit(web::cli(Some("up"))),
        Some(WebAction::Down) => std::process::exit(web::cli(Some("down"))),
        Some(WebAction::Serve) => rt()?.block_on(web::hub::serve(config)),
        Some(WebAction::Host { id, cwd, resume }) => {
            let kind = match resume {
                Some(sid) if !sid.is_empty() => SessionKind::Load(sid),
                _ => SessionKind::New,
            };
            markdown::preload();
            rt()?.block_on(web::host::run_headless(config, dunce_canon(&cwd), kind, id))
        }
    }
}

fn dunce_canon(p: &std::path::Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // Strip Windows' \\?\ verbatim prefix.
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    }
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal(mouse: bool) -> Result<(Term, bool)> {
    terminal::enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste, cursor::SetCursorStyle::SteadyBar)?;
    // Focus reports tell notifications whether you're looking at blush.
    let _ = execute!(out, EnableFocusChange);
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    let enhanced = matches!(terminal::supports_keyboard_enhancement(), Ok(true));
    if enhanced {
        let _ = execute!(
            out,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        );
    }
    let term = Terminal::new(CrosstermBackend::new(out))?;
    Ok((term, enhanced))
}

fn restore_terminal(enhanced: bool, mouse: bool) {
    let mut out = io::stdout();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    if mouse {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, DisableFocusChange);
    let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen, cursor::SetCursorStyle::DefaultUserShape, cursor::Show);
    let _ = terminal::disable_raw_mode();
}

async fn run_tui(
    config: Config,
    cwd: PathBuf,
    startup: SessionKind,
    initial_prompt: Option<String>,
    continue_latest: bool,
    pick_session: bool,
) -> Result<()> {
    let mouse = config.ui.mouse;
    let (mut term, mut enhanced) = setup_terminal(mouse)?;
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // A panic in a background task is caught by tokio and the UI keeps
            // running, so only tear the terminal down when it's fatal: on the
            // thread driving the UI. Anything else is logged instead.
            if std::thread::current().name() == Some("main") {
                restore_terminal(true, true);
                default_hook(info);
            } else {
                let log = config::home_dir().join("logs").join("panic.log");
                let _ = std::fs::create_dir_all(log.parent().unwrap());
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                    let _ = writeln!(f, "[{}] {info}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                }
            }
        }));
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AppMsg>();
    let mut app = App::new(config, cwd, tx.clone(), startup);
    app.continue_latest = continue_latest;
    app.pick_session = pick_session;
    if let Some(p) = initial_prompt {
        app.pending_prompt = Some(p);
    }
    app.start_agent();
    if app.config.web.share {
        web::host::spawn_bridge(tx.clone(), web::host::new_host_id(), "tui", false);
    }

    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(50));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_title = String::new();

    // Run the loop in a block so that an I/O error from drawing still falls
    // through to the agent shutdown and terminal restore below.
    let result: Result<()> = async {
        loop {
            if app.take_force_redraw() {
                term.clear()?;
            }
            if app.dirty {
                app.dirty = false;
                term.draw(|f| ui::draw(f, &mut app))?;
                let title = if app.responding {
                    format!("Responding - {}", app.terminal_title)
                } else {
                    app.terminal_title.clone()
                };
                if title != last_title {
                    last_title = title;
                    let _ = execute!(io::stdout(), SetTitle(&last_title));
                }
            }
            tokio::select! {
                ev = events.next() => {
                    match ev {
                        Some(Ok(ev)) => {
                            // Take everything that's already waiting: on Windows a
                            // paste arrives as a flood of key presses, which the
                            // app turns back into one paste.
                            let mut batch = vec![ev];
                            while let Some(Some(Ok(more))) = events.next().now_or_never() {
                                batch.push(more);
                            }
                            app.on_events(batch).await;
                        }
                        Some(Err(_)) | None => break,
                    }
                }
                Some(msg) = rx.recv() => {
                    app.on_msg(msg).await;
                    while let Ok(m) = rx.try_recv() {
                        app.on_msg(m).await;
                    }
                }
                _ = ticker.tick() => {
                    app.tick = app.tick.wrapping_add(1);
                    app.web_sync();
                    if app.tick % 1200 == 0 {
                        // Once a minute: keep the subscription meter fresh while idle.
                        app.maybe_refresh_usage();
                    }
                    if app.tick % 40 == 0 {
                        // Every 2s: pick up branch switches for the header.
                        let label = crate::ui::git_label(&app.cwd);
                        if label != app.git_label {
                            app.git_label = label;
                            app.dirty = true;
                        }
                    }
                    let toast_live = app.toast.as_ref().is_some_and(|(_, t)| t.elapsed() < Duration::from_millis(2700));
                    // Every 6s: the subscription meter alternates 5h / weekly.
                    let meter_flip = app.tick % 120 == 0 && !app.limits.is_empty();
                    if app.animating() || toast_live || meter_flip || app.poll_narrator() {
                        app.dirty = true;
                    }
                }
            }
            if app.should_quit {
                break;
            }
            if let Some(ext) = app.external.take() {
                restore_terminal(enhanced, mouse);
                match ext {
                    External::Login => {
                        println!("\nRunning `claude auth login` — follow the prompts, then you'll return to blush.\n");
                        let status = std::process::Command::new(which::which("claude").unwrap_or_else(|_| "claude".into()))
                            .args(["auth", "login"])
                            .status();
                        if let Err(e) = status {
                            println!("Couldn't run claude: {e}. Install Claude Code: https://claude.com/code");
                            println!("Press Enter to return…");
                            let _ = io::stdin().read_line(&mut String::new());
                        }
                    }
                }
                let (t, e) = setup_terminal(mouse)?;
                term = t;
                enhanced = e;
                term.clear()?;
                app.dirty = true;
                app.after_login().await;
            }
        }
        Ok(())
    }
    .await;

    if let Some(c) = app.client.take() {
        c.shutdown().await;
    }
    restore_terminal(enhanced, mouse);
    let _ = execute!(io::stdout(), SetTitle(""));
    if let Some(id) = &app.session.id {
        let _ = writeln!(io::stdout(), "Resume this session with: blush --resume {id}");
    }
    result
}
