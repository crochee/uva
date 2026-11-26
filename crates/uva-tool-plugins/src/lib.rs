//! # uva-tool-plugins
//!
//! The shipped **tool plugins** — one module and one feature per tool.
//! The paradigm (the [`Tool`](uva_tool::Tool) trait, the registry,
//! the exposure projection) lives in `uva-tool`; this crate ships
//! the *implementations*, so a consumer composes the tool surface brick
//! by brick instead of depending on a whole crate per brick.
//!
//! | feature | module | what the tool does |
//! |---|---|---|
//! | `read` | [`mod@read`] | read a workspace file, with an optional line range |
//! | `write` | [`mod@write`] | write a workspace file |
//! | `edit` | [`mod@edit`] | replace an exact piece of a workspace file's text |
//! | `glob` | [`mod@glob`] | find the files that match a pattern |
//! | `grep` | [`mod@grep`] | search the files' contents |
//! | `job` | [`mod@jobs`] | run a command in the background and come back to it |
//! | `shell` | [`mod@shell`] | run a process, under the OS execution policy it owns |
//! | `todo` | [`mod@todo`] | the `TodoWrite` task list |
//! | `web` | [`mod@web`] | fetch a URL — the only tool that pulls an HTTP client |
//! | `scheduler` | [`mod@scheduler`] | schedule a job |
//! | `search` | [`mod@search`] | query the host's cross-domain search registry |
//! | `memo` | [`mod@memo`] | memoise any tool's results |
//!
//! The [`files`](mod@files) module is not a tool: it is the seam the
//! `read` and `write` tools read through — the owner of the files a
//! session's tools touch (the deployment's disk by default, a
//! client's workspace when the client owns it).
//!
//! ## Composing a tool surface
//!
//! Each tool ships as a **kernel plugin** that registers itself in the
//! injected `tools` registry during `apply`, and withdraws exactly that
//! registration when it is unloaded. A consumer mounts the plugins it
//! wants; the registry ends up holding exactly those tools.
//!
//! ## Dependencies travel with their tool
//!
//! A tool's own dependencies are optional and are opened by that tool's
//! feature: `web` is the only feature that brings an HTTP client, and
//! the default build (no feature) pulls no tool at all.
//!
//! The `task` tool — the one that runs a registered peer agent as a
//! child run — lives in its own crate, [`uva-tool-task`](../uva-tool-task),
//! because it needs the agent vocabulary rather than the tool trait
//! alone.

#[cfg(feature = "edit")]
pub use edit::{EditTool, EditToolPlugin};
pub use files::{FileError, FileSystemOwner, LocalFileSystem};
#[cfg(feature = "glob")]
pub use glob::{GLOB_MAX_RESULTS, GlobTool, GlobToolPlugin};
#[cfg(feature = "grep")]
pub use grep::{GREP_MAX_MATCHES, GrepTool, GrepToolPlugin};
#[cfg(feature = "job")]
pub use jobs::{JobRegistry, JobState, JobTool, JobToolPlugin};
#[cfg(feature = "memo")]
pub use memo::{
    InMemoryMemoStore,
    MemoConfig,
    MemoEntry,
    MemoKey,
    MemoScope,
    MemoStore,
    MemoizedTool,
};
#[cfg(feature = "read")]
pub use read::{ReadTool, ReadToolPlugin};
#[cfg(feature = "scheduler")]
pub use scheduler::{
    MAX_JOBS,
    SCHEDULE_TOOL_NAME,
    ScheduleToolPlugin,
    SchedulerTool,
    register_scheduler_tool,
};
#[cfg(feature = "search")]
pub use search::{
    SEARCH_TOOL_NAME,
    SearchTool,
    register_search_tool,
    search_tool_entry,
};
#[cfg(feature = "shell")]
pub use shell::{
    DEFAULT_DENY_PATTERNS,
    DEFAULT_TIMEOUT_SECS,
    MAX_TIMEOUT_SECS,
    ShellSafetyConfig,
    ShellTool,
    ShellToolPlugin,
};
#[cfg(feature = "todo")]
pub use todo::{TodoWriteTool, TodoWriteToolPlugin};
#[cfg(feature = "web")]
pub use web::{WebFetchTool, WebFetchToolPlugin};
#[cfg(feature = "write")]
pub use write::{WriteTool, WriteToolPlugin};

#[cfg(feature = "edit")]
pub mod edit;
/// The file-system seam the `read` and `write` tools read through.
pub mod files;
#[cfg(feature = "glob")]
pub mod glob;
#[cfg(feature = "grep")]
pub mod grep;
#[cfg(feature = "job")]
pub mod jobs;
#[cfg(feature = "memo")]
pub mod memo;
/// The one way this crate spawns a process (unix only).
#[cfg(all(unix, feature = "shell"))]
pub(crate) mod process;
#[cfg(feature = "read")]
pub mod read;
#[cfg(feature = "scheduler")]
pub mod scheduler;
#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "shell")]
pub mod shell;
#[cfg(feature = "todo")]
pub mod todo;
/// The walk the search tools share: the files under a root ✓.
#[cfg(any(feature = "glob", feature = "grep"))]
pub(crate) mod walk;
#[cfg(feature = "web")]
pub mod web;
#[cfg(feature = "write")]
pub mod write;
