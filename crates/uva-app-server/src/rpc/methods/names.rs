//! The method names — one spelling each, in one place.
//!
//! Every RPC method the process answers is named here exactly once.
//! The router registers by these constants (`router.register(
//! names::SESSIONS_LIST, …)`), the front-end method gate reads this
//! file, and the terminal names its own requests with the same
//! constants — so a method name has a single spelling in the
//! workspace, and a client that calls one cannot misspell it.
//!
//! [`ALL`] is the whole table. A test pins the table against the
//! router in both directions: a method the router registers but the
//! table does not list (a new method nobody declared) and a method
//! the table lists but the router does not register (a name nobody
//! serves) are both red.
//!
//! The names are the wire's, not this module's: they are spelled the
//! way the clients call them, and a rename is a wire break for every
//! client at once.

// ---- agents, skills, tools ----------------------------------------

/// `agents/list` — list the registered agents.
pub const AGENTS_LIST: &str = "agents/list";
/// `agents/create` — register an agent.
pub const AGENTS_CREATE: &str = "agents/create";
/// `agents/default` — the default agent's name.
pub const AGENTS_DEFAULT: &str = "agents/default";
/// `agents/get` — one agent by name.
pub const AGENTS_GET: &str = "agents/get";
/// `agents/delete` — remove an agent.
pub const AGENTS_DELETE: &str = "agents/delete";
/// `skills/list` — list the registered skills.
pub const SKILLS_LIST: &str = "skills/list";
/// `skills/create` — register a skill.
pub const SKILLS_CREATE: &str = "skills/create";
/// `skills/reload` — reload the skill registry.
pub const SKILLS_RELOAD: &str = "skills/reload";
/// `skills/get` — one skill by name.
pub const SKILLS_GET: &str = "skills/get";
/// `skills/delete` — remove a skill.
pub const SKILLS_DELETE: &str = "skills/delete";
/// `tools/list` — list the registered tools.
pub const TOOLS_LIST: &str = "tools/list";
/// `tools/register` — register a tool.
pub const TOOLS_REGISTER: &str = "tools/register";
/// `tools/get` — one tool by name.
pub const TOOLS_GET: &str = "tools/get";
/// `tools/unregister` — remove a tool.
pub const TOOLS_UNREGISTER: &str = "tools/unregister";

// ---- automation ---------------------------------------------------

/// `schedules/list` — list the schedules.
pub const SCHEDULES_LIST: &str = "schedules/list";
/// `schedules/create` — create a schedule.
pub const SCHEDULES_CREATE: &str = "schedules/create";
/// `schedules/get` — one schedule by id.
pub const SCHEDULES_GET: &str = "schedules/get";
/// `schedules/patch` — change a schedule.
pub const SCHEDULES_PATCH: &str = "schedules/patch";
/// `schedules/delete` — remove a schedule.
pub const SCHEDULES_DELETE: &str = "schedules/delete";
/// `schedules/tick` — fire a schedule now.
pub const SCHEDULES_TICK: &str = "schedules/tick";
/// `tasks/list` — list the scheduled tasks.
pub const TASKS_LIST: &str = "tasks/list";
/// `tasks/get` — one task by id.
pub const TASKS_GET: &str = "tasks/get";
/// `evals/list` — list the evaluations.
pub const EVALS_LIST: &str = "evals/list";
/// `evals/create` — create an evaluation.
pub const EVALS_CREATE: &str = "evals/create";
/// `evals/get` — one evaluation by name.
pub const EVALS_GET: &str = "evals/get";
/// `evals/delete` — remove an evaluation.
pub const EVALS_DELETE: &str = "evals/delete";
/// `evals/run` — run an evaluation.
pub const EVALS_RUN: &str = "evals/run";

// ---- chat ---------------------------------------------------------

/// `chat/session/create` — create a chat session.
pub const CHAT_SESSION_CREATE: &str = "chat/session/create";
/// `chat/message/send` — send a message into a session.
pub const CHAT_MESSAGE_SEND: &str = "chat/message/send";
/// `chat/cancel` — cancel the running turn.
pub const CHAT_CANCEL: &str = "chat/cancel";
/// `chat/regenerate` — run the turn again.
pub const CHAT_REGENERATE: &str = "chat/regenerate";
/// `chat/feedback` — rate an answer.
pub const CHAT_FEEDBACK: &str = "chat/feedback";
/// `chat/usage` — the chat surface's token usage.
pub const CHAT_USAGE: &str = "chat/usage";
/// `chat/operation` — a control operation on a session.
pub const CHAT_OPERATION: &str = "chat/operation";

// ---- sessions -----------------------------------------------------

/// `sessions/list` — list the sessions.
pub const SESSIONS_LIST: &str = "sessions/list";
/// `sessions/get` — one session by id.
pub const SESSIONS_GET: &str = "sessions/get";
/// `sessions/delete` — remove a session.
pub const SESSIONS_DELETE: &str = "sessions/delete";
/// `sessions/export` — export a session's transcript.
pub const SESSIONS_EXPORT: &str = "sessions/export";
/// `sessions/fork` — fork a session.
pub const SESSIONS_FORK: &str = "sessions/fork";
/// `sessions/events` — the durable rows of a session.
pub const SESSIONS_EVENTS: &str = "sessions/events";
/// `sessions/status` — a session's status.
pub const SESSIONS_STATUS: &str = "sessions/status";
/// `sessions/follow` — follow a session's projection.
pub const SESSIONS_FOLLOW: &str = "sessions/follow";
/// `session/unfollow` — stop following a session.
pub const SESSION_UNFOLLOW: &str = "session/unfollow";
/// `sessions/resume` — resume a suspended session.
pub const SESSIONS_RESUME: &str = "sessions/resume";
/// `approval/respond` — answer a held call.
pub const APPROVAL_RESPOND: &str = "approval/respond";
/// `sessions/compact` — compact a session's transcript.
pub const SESSIONS_COMPACT: &str = "sessions/compact";
/// `sessions/usage` — a session's token usage.
pub const SESSIONS_USAGE: &str = "sessions/usage";
/// `sessions/search` — search the sessions.
pub const SESSIONS_SEARCH: &str = "sessions/search";

// ---- profile ------------------------------------------------------

/// `profile/list` — list the profiles.
pub const PROFILE_LIST: &str = "profile/list";
/// `profile/get` — one profile by name.
pub const PROFILE_GET: &str = "profile/get";
/// `profile/set` — set a profile's value.
pub const PROFILE_SET: &str = "profile/set";
/// `profile/create` — create a profile.
pub const PROFILE_CREATE: &str = "profile/create";
/// `profile/delete` — remove a profile.
pub const PROFILE_DELETE: &str = "profile/delete";

// ---- misc ---------------------------------------------------------

/// `health/ping` — the liveness probe.
pub const HEALTH_PING: &str = "health/ping";
/// `health/version` — the build's version.
pub const HEALTH_VERSION: &str = "health/version";
/// `health/livez` — the liveness gate.
pub const HEALTH_LIVEZ: &str = "health/livez";
/// `health/readyz` — the readiness gate.
pub const HEALTH_READYZ: &str = "health/readyz";
/// `models/list` — the provider catalogue.
pub const MODELS_LIST: &str = "models/list";
/// `ui/schema` — the deployment's UI slots.
pub const UI_SCHEMA: &str = "ui/schema";
/// `anthropic/messages` — the Anthropic compatibility endpoint.
pub const ANTHROPIC_MESSAGES: &str = "anthropic/messages";
/// `search/all` — the cross-domain search.
pub const SEARCH_ALL: &str = "search/all";
/// `attachments/create` — store an attachment.
pub const ATTACHMENTS_CREATE: &str = "attachments/create";
/// `attachments/get` — read an attachment.
pub const ATTACHMENTS_GET: &str = "attachments/get";
/// `attachments/delete` — remove an attachment.
pub const ATTACHMENTS_DELETE: &str = "attachments/delete";
/// `metrics/gather` — this process's Prometheus exposition.
///
/// The gateway scrapes the core through it: the core runs the agent
/// loop, so the tool counters live in the core's process, while the
/// HTTP counters live in the gateway's. The `/metrics` endpoint is
/// the two together.
pub const METRICS_GATHER: &str = "metrics/gather";

/// Every method name the process answers.
///
/// The table the router's registrations and the clients' method
/// names are both checked against.
pub const ALL: &[&str] = &[
    // agents, skills, tools
    AGENTS_LIST,
    AGENTS_CREATE,
    AGENTS_DEFAULT,
    AGENTS_GET,
    AGENTS_DELETE,
    SKILLS_LIST,
    SKILLS_CREATE,
    SKILLS_RELOAD,
    SKILLS_GET,
    SKILLS_DELETE,
    TOOLS_LIST,
    TOOLS_REGISTER,
    TOOLS_GET,
    TOOLS_UNREGISTER,
    // automation
    SCHEDULES_LIST,
    SCHEDULES_CREATE,
    SCHEDULES_GET,
    SCHEDULES_PATCH,
    SCHEDULES_DELETE,
    SCHEDULES_TICK,
    TASKS_LIST,
    TASKS_GET,
    EVALS_LIST,
    EVALS_CREATE,
    EVALS_GET,
    EVALS_DELETE,
    EVALS_RUN,
    // chat
    CHAT_SESSION_CREATE,
    CHAT_MESSAGE_SEND,
    CHAT_CANCEL,
    CHAT_REGENERATE,
    CHAT_FEEDBACK,
    CHAT_USAGE,
    CHAT_OPERATION,
    // sessions
    SESSIONS_LIST,
    SESSIONS_GET,
    SESSIONS_DELETE,
    SESSIONS_EXPORT,
    SESSIONS_FORK,
    SESSIONS_EVENTS,
    SESSIONS_STATUS,
    SESSIONS_FOLLOW,
    SESSION_UNFOLLOW,
    SESSIONS_RESUME,
    APPROVAL_RESPOND,
    SESSIONS_COMPACT,
    SESSIONS_USAGE,
    SESSIONS_SEARCH,
    // profile
    PROFILE_LIST,
    PROFILE_GET,
    PROFILE_SET,
    PROFILE_CREATE,
    PROFILE_DELETE,
    // misc
    HEALTH_PING,
    HEALTH_VERSION,
    HEALTH_LIVEZ,
    HEALTH_READYZ,
    MODELS_LIST,
    UI_SCHEMA,
    ANTHROPIC_MESSAGES,
    SEARCH_ALL,
    ATTACHMENTS_CREATE,
    ATTACHMENTS_GET,
    ATTACHMENTS_DELETE,
    METRICS_GATHER,
];
