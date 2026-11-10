//! The desktop shell.
//!
//! Four jobs and nothing else (design knife 3):
//!
//! 1. start the sidecar (`elysper-app-server serve`) via the shared
//!    gateway supervisor and wait for `/readyz`;
//! 2. open one window at the gateway's `base_url` (external URL, so
//!    the page origin and the WebSocket origin are the same);
//! 3. when the sidecar dies, the supervisor restarts it — and if the
//!    new generation lands on a different port, navigate the window;
//! 4. when the window closes, reap the sidecar (no orphans).
//!
//! The shell speaks no protocol: JSON-RPC belongs to the gateway and
//! the SPA. The webview shows the same `elysper-web/dist` the
//! browser does.

#![forbid(unsafe_code)]

pub mod options;
#[cfg(test)]
mod tests;
pub mod window_nav;

use std::{path::Path, time::Duration};

use anyhow::Context as _;
use elysper_gateway::{GatewayOptions, GatewayState};
use tauri::Manager as _;

/// How often the state watcher checks the supervisor's narration
/// channel. The supervisor emits one state per transition, so a
/// short poll keeps a post-restart redirect snappy without
/// busy-spinning. There is no async consumer here on purpose: the
/// dependency whitelist (design D14) has no `futures` in it, and
/// `try_recv` is an inherent method of the channel the gateway
/// hands out.
const STATE_POLL: Duration = Duration::from_millis(100);

/// The dev-session window address (design D2): the Vite dev server,
/// whose proxy already forwards `/rpc` (ws) and `/readyz`.
const DEV_URL: &str = "http://localhost:5173";

/// Tauri managed state: lets the `RunEvent::Exit` handler reach the
/// supervisor. `Gateway` also shuts the child down on drop — the
/// explicit call is the belt to that braces.
struct GatewayHandle(elysper_gateway::Gateway);

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = options::Env {
        app_server: std::env::var("ELYSPER_APP_SERVER").ok(),
        backend_port: std::env::var("ELYSPER_BACKEND_PORT").ok(),
    };
    let options =
        options::Options::parse_from(&args, &env).unwrap_or_else(|usage| {
            eprintln!("{usage}");
            std::process::exit(2);
        });

    tauri::Builder::default()
        .setup(move |app| {
            // 1. Locate the sidecar and the dist. The pure functions
            //    in `options` order the candidates; the IO lives here.
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()));
            let executable = options::first_existing(
                &options::app_server_candidates(
                    options.app_server.as_deref(),
                    exe_dir.as_deref(),
                ),
                Path::is_file,
            )
            .cloned()
            .context(
                "no elysper-app-server found: pass --app-server, set \
                 ELYSPER_APP_SERVER, or add it to PATH",
            )?;
            let resource_dist =
                app.path().resource_dir().ok().map(|dir| dir.join("dist"));
            let web_dir = options::first_existing(
                &options::web_dir_candidates(
                    options.web_dir.as_deref(),
                    resource_dist.as_deref(),
                ),
                Path::is_dir,
            )
            .cloned()
            .context(
                "no web dist found: run `make -C desktop frontend` or \
                 pass --web-dir",
            )?;

            // 2. Assemble the gateway options and wait for readiness.
            //    The supervisor owns D10's 30s budget (it answers with
            //    `GatewayError::NotReady`), so the shell does not wrap
            //    a second timeout around it.
            let is_release = !cfg!(debug_assertions);
            let port =
                options::gateway_port(is_release, env.backend_port.as_deref())
                    .map_err(|message| anyhow::anyhow!(message))?;
            let gateway_options = GatewayOptions {
                executable,
                bind: "127.0.0.1".to_owned(),
                port,
                directory: options.directory.clone(),
                profile: options.profile.clone(),
                bundle: options.bundle.clone(),
                patch: options.patch.clone(),
                web_dir: Some(web_dir),
            };
            let gateway = tauri::async_runtime::block_on(
                elysper_gateway::Gateway::start(gateway_options),
            )
            .map_err(|error| {
                anyhow::anyhow!("failed to start the gateway sidecar: {error}")
            })?;

            // 3. Open the window: release loads the gateway's
            //    `base_url` (page origin == WS origin, design D1);
            //    debug loads the dev address (design D2).
            let dev_url = options
                .dev_url
                .clone()
                .or_else(|| (!is_release).then(|| DEV_URL.to_owned()));
            let initial = dev_url
                .clone()
                .unwrap_or_else(|| gateway.base_url().to_owned());
            tauri::webview::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(
                    initial.parse().context("window url")?,
                ),
            )
            .title("Elysper")
            .build()
            .context("failed to open the window")?;

            // 4. Watch the state narration: a `Ready` on a different
            //    address re-navigates the window. Only this thread
            //    reads or writes the bookkeeping, so it owns it
            //    outright; window calls are dispatched to the event
            //    loop, so a plain thread is safe.
            app.manage(GatewayHandle(gateway));
            let handle = app.handle().clone();
            let mut states = app.state::<GatewayHandle>().0.states();
            std::thread::spawn(move || {
                let mut shown = initial;
                loop {
                    match states.try_recv() {
                        Ok(state) => {
                            let GatewayState::Ready { base_url, .. } = state
                            else {
                                continue;
                            };
                            let Some(next) = window_nav::next_window_url(
                                Some(shown.as_str()),
                                dev_url.as_deref(),
                                &base_url,
                            ) else {
                                continue;
                            };
                            let Ok(url) = next.parse() else {
                                continue;
                            };
                            let Some(window) =
                                handle.get_webview_window("main")
                            else {
                                continue;
                            };
                            if window.navigate(url).is_ok() {
                                shown = next;
                            }
                        }
                        // `Empty` — and, in theory, `Closed`. The
                        // sender lives exactly as long as the app's
                        // managed state, so a closed channel
                        // coincides with process exit; telling the
                        // two apart would mean naming futures'
                        // error type, which the dependency
                        // whitelist (D14) does not carry.
                        Err(_) => std::thread::sleep(STATE_POLL),
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the desktop shell")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(handle) = app.try_state::<GatewayHandle>() {
                    // Explicit reap; `Gateway`'s Drop backs it up.
                    handle.0.shutdown();
                }
            }
        });
}
