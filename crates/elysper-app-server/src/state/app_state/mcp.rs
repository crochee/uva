//! The MCP supervisor's life as a boot-owned component.
//!
//! [`McpRuntime`] owns three things the `mcp` plugin deliberately does
//! not: the supervisor it published, the live clients whose transports
//! (and child processes) must stay alive, and the reconnect tick loop.
//! The plugin's only effect is the service registration, so unloading
//! it stops the loop at its next service read; [`McpRuntime::shutdown`]
//! is the explicit teardown the unload path runs so tool withdrawal does
//! not wait for that read.
//!
//! # Tick ownership
//!
//! [`McpSupervisor::tick`](elysper::mcp::McpSupervisor::tick) is a
//! clock-injected call; the crate never spawns a timer. The loop here
//! arms the next wake from the deadline the tick returned and re-reads
//! the service each wake, so an unload is observed rather than assumed.

use std::{sync::Arc, time::Duration};

use elysper::{
    core::{Clock as _, SharedClock},
    kernel::Context,
    mcp::{KEY_MCP, McpClient, McpSupervisor, ServerHealth},
    tool::ToolRegistry,
};
use tokio::{sync::watch, task::JoinHandle};

/// How often the loop wakes when the supervisor scheduled nothing.
///
/// It is also the bound on noticing a service withdrawn without the
/// explicit [`McpRuntime::shutdown`] path: the unload path signals the
/// loop, this cap only keeps a direct handle drop from lingering
/// forever on a timer.
const IDLE_WAIT: Duration = Duration::from_secs(60);

/// The supervisor, its live clients, and the reconnect loop.
pub struct McpRuntime {
    supervisor: parking_lot::Mutex<Option<Arc<McpSupervisor>>>,
    clients: parking_lot::Mutex<Vec<(String, Arc<McpClient>)>>,
    health: parking_lot::Mutex<Vec<ServerHealth>>,
    task: parking_lot::Mutex<Option<JoinHandle<()>>>,
    stop: watch::Sender<bool>,
}

impl McpRuntime {
    /// Start the tick loop over a mounted `mcp` service.
    ///
    /// The first tick runs immediately (the boot's registration sweep
    /// has already connected the servers; this keeps a test or a
    /// remount from waiting out `IDLE_WAIT` before the first
    /// supervision pass).
    pub(crate) fn start(
        kernel: Context,
        registry: Arc<ToolRegistry>,
        supervisor: Arc<McpSupervisor>,
        clients: Vec<(String, Arc<McpClient>)>,
        health: Vec<ServerHealth>,
        clock: SharedClock,
    ) -> Self {
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut next = Some(clock.now());
            loop {
                let wait = match next {
                    Some(deadline) => (deadline - clock.now())
                        .to_std()
                        .unwrap_or(Duration::ZERO),
                    None => IDLE_WAIT,
                };
                let wake = tokio::time::sleep(wait);
                tokio::pin!(wake);
                tokio::select! {
                    () = &mut wake => {}
                    changed = stopped.changed() => {
                        if changed.is_err() || *stopped.borrow() {
                            break;
                        }
                        continue;
                    }
                }
                let Ok(Some(service)) = kernel.get::<McpSupervisor>(KEY_MCP)
                else {
                    // The service was withdrawn: the plugin is
                    // unloaded or the Context is gone. Stop ticking.
                    break;
                };
                let tick = service.tick(&registry, clock.now()).await;
                next = tick.next_deadline;
            }
        });
        Self {
            supervisor: parking_lot::Mutex::new(Some(supervisor)),
            clients: parking_lot::Mutex::new(clients),
            health: parking_lot::Mutex::new(health),
            task: parking_lot::Mutex::new(Some(task)),
            stop,
        }
    }

    /// A runtime with no loop and no servers — the fixture shape.
    #[cfg_attr(not(any(test, feature = "test-utils")), allow(dead_code))]
    pub(crate) fn detached(supervisor: Arc<McpSupervisor>) -> Self {
        let (stop, _receiver) = watch::channel(false);
        Self {
            supervisor: parking_lot::Mutex::new(Some(supervisor)),
            clients: parking_lot::Mutex::new(Vec::new()),
            health: parking_lot::Mutex::new(Vec::new()),
            task: parking_lot::Mutex::new(None),
            stop,
        }
    }

    /// The supervisor this runtime drives, or `None` after shutdown.
    #[must_use]
    pub fn supervisor(&self) -> Option<Arc<McpSupervisor>> {
        self.supervisor.lock().clone()
    }

    /// The live clients kept alive for the process's MCP servers.
    #[must_use]
    pub fn clients(&self) -> Vec<(String, Arc<McpClient>)> {
        self.clients.lock().clone()
    }

    /// The boot sweep's per-server health snapshot.
    #[must_use]
    pub fn health(&self) -> Vec<ServerHealth> {
        self.health.lock().clone()
    }

    /// Whether the tick loop is still owned by this runtime.
    #[must_use]
    pub fn is_ticking(&self) -> bool {
        self.task.lock().is_some()
    }

    /// Stop the loop, withdraw every remote tool generation, and drop
    /// the supervisor and its clients (which terminates every transport
    /// and child process).
    ///
    /// Idempotent: a second call finds no supervisor and no task.
    pub async fn shutdown(&self, registry: &ToolRegistry) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.lock().take() {
            task.abort();
        }
        let supervisor = self.supervisor.lock().take();
        if let Some(supervisor) = supervisor {
            supervisor.shutdown(registry).await;
        }
        self.clients.lock().clear();
        self.health.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use elysper::{
        core::SharedClock,
        kernel::{Context, Plugin as KernelPlugin, mount_all},
        mcp::{
            InMemoryTransport,
            InMemoryTransportFactory,
            KEY_MCP,
            McpPlugin,
            McpSupervisor,
        },
        tool::{ToolRegistry, ToolRegistryPlugin},
    };

    use super::McpRuntime;

    #[tokio::test]
    async fn shutdown_stops_the_tick_and_unregisters_remote_tools() {
        let ctx = Context::new();
        let wire = Arc::new(InMemoryTransport::new().with_echo_server());
        let registry = Arc::new(ToolRegistry::new());
        let _mounted = mount_all(
            &ctx,
            &[
                Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                    as Arc<dyn KernelPlugin>,
                Arc::new(McpPlugin::new()) as Arc<dyn KernelPlugin>,
            ],
        )
        .expect("mount tools + mcp");
        // The supervisor the plugin published is the one the runtime
        // drives; the test supervises its in-memory server on it.
        let supervisor = ctx
            .require::<elysper::mcp::McpSupervisor>(elysper::mcp::KEY_MCP)
            .expect("mcp service");
        supervisor
            .supervise(
                "srv",
                Arc::new(InMemoryTransportFactory::new(Arc::clone(&wire))) as _,
            )
            .await;

        let runtime = McpRuntime::start(
            ctx.clone(),
            Arc::clone(&registry),
            Arc::clone(&supervisor),
            Vec::new(),
            Vec::new(),
            SharedClock::system(),
        );
        let mut registered = false;
        for _ in 0..100 {
            if registry.contains("mcp__srv__echo") {
                registered = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(registered, "the tick loop registered the remote tool");

        runtime.shutdown(&registry).await;
        assert!(
            !registry.contains("mcp__srv__echo"),
            "shutdown withdraws the generation's tools"
        );
        assert!(!runtime.is_ticking(), "the tick loop is stopped");
        assert!(
            runtime.supervisor().is_none(),
            "the supervisor is dropped so its transports terminate"
        );
        assert!(
            ctx.get::<McpSupervisor>(KEY_MCP)
                .expect("read service")
                .is_some(),
            "the plugin's service registration is the unload path's job"
        );
    }
}
