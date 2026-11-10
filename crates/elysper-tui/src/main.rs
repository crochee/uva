//! `elysper-tui`: the terminal projection's entry point.
//!
//! The loop is three independent sources under `tokio::select!`:
//!
//! 1. crossterm's [`EventStream`] — key input;
//! 2. the transport's event stream — the core's frames and lifecycle;
//! 3. a render tick — keeps the screen alive while nothing arrives.
//!
//! The reducer ([`App`]) turns both input and transport events into
//! actions; this file owns the one place that writes them to the core
//! (and the one local action, the clipboard copy). The terminal guard
//! restores raw mode and the alternate screen on both a normal exit
//! and a panic.

use std::{io, path::PathBuf, time::Duration};

use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture,
        EnableMouseCapture,
        Event,
        EventStream,
        KeyCode,
        KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{
        EnterAlternateScreen,
        LeaveAlternateScreen,
        disable_raw_mode,
        enable_raw_mode,
    },
};
use elysper_gateway::GatewayOptions;
use elysper_tui::{
    app::{Action, App},
    render,
    transport::{Driver, RequestKind, TransportEvent},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::time::MissedTickBehavior;

/// How often the screen is repainted while nothing arrives.
const RENDER_TICK: Duration = Duration::from_millis(80);

#[derive(Parser, Debug)]
#[command(
    name = "elysper-tui",
    about = "A terminal projection of the elysper gateway",
    version
)]
struct Args {
    /// The workspace the gateway serves (passed as `--directory`).
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,
    /// The plugin profile the gateway resolves (passed as `--profile`).
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
    /// The `elysper-app-server` binary (which runs `serve`).
    /// Defaults to `elysper-app-server` on `PATH`.
    #[arg(long, env = "ELYSPER_APP_SERVER", value_name = "PATH")]
    app_server: Option<PathBuf>,
    /// Attach to a gateway that is already running instead of
    /// launching one (`http://host:port` or `ws://host:port/rpc`).
    #[arg(long, value_name = "URL")]
    attach: Option<String>,
}

/// Raw mode + alternate screen, restored when this is dropped.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ =
            execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// A panic inside the loop must not leave the user's terminal in raw
/// mode with no cursor. The guard's `Drop` is not reached on a panic
/// (the unwinder runs drops, but the hook runs first), so the hook
/// restores the terminal before the default hook prints the panic.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ =
            execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        previous(info);
    }));
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    install_panic_hook();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    // `--attach` connects to a running gateway and never spawns one;
    // otherwise the TUI launches `serve` itself.
    let mut driver = match args.attach.as_deref() {
        Some(url) => Driver::attach(url),
        None => {
            let executable = args
                .app_server
                .unwrap_or_else(|| PathBuf::from("elysper-app-server"));
            let options = GatewayOptions {
                profile: args.profile,
                ..GatewayOptions::new(executable, args.directory)
            };
            Driver::launch(options)
        }
    };
    let sender = driver.sender();

    let mut app = App::new();
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(RENDER_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    terminal.draw(|frame| render::draw(frame, &app))?;
    loop {
        // The list's half-page step and viewport scroll need the row
        // count of the pane they live in; the chrome (status bar, the
        // table's border, header and status rows) is the rest.
        if let Ok(size) = terminal.size() {
            app.set_viewport(size.height.saturating_sub(5).max(1));
        }
        let actions = tokio::select! {
            maybe = input.next() => match maybe {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c');
                    if app.catalog().palette_open() && !ctrl_c {
                        // The real command palette floats above any
                        // pane; `Ctrl+c` still belongs to the loop.
                        app.handle_palette_key(key)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('k')
                        && app.approval().is_none()
                        && app.overlay().is_none()
                    {
                        app.catalog_mut().open_palette();
                        Vec::new()
                    } else {
                        app.handle_key(key)
                    }
                }
                Some(Err(error)) => {
                    app.note_error(format!("terminal input: {error}"));
                    Vec::new()
                }
                _ => Vec::new(),
            },
            maybe = driver.recv() => match maybe {
                // Directory answers bypass the reducer (`Review
                // Focus #5`): folded into the catalog here, they can
                // never overwrite the connection status line.
                Some(TransportEvent::Response {
                    request: Some(RequestKind::Catalog(method)),
                    result,
                    ..
                }) => {
                    app.catalog_mut().apply_response(method, &result);
                    // A write's answer wants the listing reloaded; the
                    // refresh request rides the same drain as the
                    // keys' requests.
                    let refresh = app.catalog_mut().refresh();
                    app.enqueue_catalog_requests(refresh);
                    Vec::new()
                }
                Some(event) => app.apply(event),
                None => {
                    app.note_error("the transport supervisor stopped");
                    app.quit();
                    Vec::new()
                }
            },
            _ = tick.tick() => Vec::new(),
        };
        for action in actions {
            if let Action::CopyId { id } = &action {
                copy_to_clipboard(id);
                continue;
            }
            if let Some(request) = action.request()
                && let Err(error) = sender.send(&request).await
            {
                app.note_error(format!("transport: {error}"));
            }
        }
        for request in app.take_catalog_requests() {
            if let Err(error) = sender.send(&request).await {
                app.catalog_mut().note_send_failure(&error.to_string());
            }
        }
        terminal.draw(|frame| render::draw(frame, &app))?;
        if app.is_quit() {
            break;
        }
    }
    Ok(())
}

/// Copy text to the terminal's clipboard with OSC 52 — the one
/// clipboard channel every modern terminal and multiplexer speaks,
/// with no platform crate and no window to own. Terminals that
/// ignore it simply do nothing.
fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;

    use base64::Engine as _;

    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{encoded}\x07");
    let _ = stdout.flush();
}
