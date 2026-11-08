/**
 * Shared types for the elysper-web frontend.
 *
 * Mirrors the v1 Management API wire format served by
 * `elysper-server` at `/api/v1/*`. Field names match the JSON
 * serialization (camelCase where the Rust server serializes
 * camelCase via `#[serde(rename_all = "camelCase")]`).
 *
 * The v1 API returns resources directly as top-level JSON (no
 * `{ status, data }` envelope). List endpoints return the
 * generic `List<T>` shape; detail endpoints return the resource
 * type itself.
 */

/**
 * Generic list response envelope used by every v1 list endpoint.
 *
 * `next_cursor` is `null`/absent when there are no more pages.
 * `total` is `null`/absent when the server cannot cheaply compute
 * a count (e.g. large datasets); clients must not assume it is
 * always present.
 */
export interface List<T> {
  data: T[];
  next_cursor?: string | null;
  /** Present (true) when the endpoint caps results without a
   *  cursor — e.g. `/api/v1/sessions/search`, where the search
   *  owns best-first ordering so pagination is meaningless. */
  has_more?: boolean | null;
  total?: number | null;
}

export interface Skill {
  name: string;
  description?: string;
}

/**
 * Full skill detail returned by `GET /api/v1/skills/:name`.
 *
 * `frontmatter` is the parsed YAML/JSON frontmatter of SKILL.md
 * exposed as a flat `key → value` map (including the nested
 * `metadata` block flattened one level deep). `body` is the
 * raw markdown body (everything after the frontmatter closer)
 * that the detail page renders as markdown.
 */
export interface SkillDetail {
  name: string;
  description: string;
  path: string;
  frontmatter: Record<string, unknown>;
  body: string;
}

export interface Tool {
  name: string;
  description?: string;
}

/**
 * Detail payload returned by `GET /api/v1/tools/{name}`.
 *
 * `provenance` is `"core"` for tools compiled into the binary
 * and `"dynamic"` for tools registered at runtime. `input_schema`
 * is the JSON Schema the LLM uses for tool_choice.
 */
export interface ToolDetail {
  name: string;
  description: string;
  input_schema: Record<string, unknown>;
  provenance: 'core' | 'dynamic';
}

/**
 * Payload returned by `GET /api/v1/agents` and
 * `GET /api/v1/agents/{name}`. The same shape is used for
 * list rows and detail rows; `protected: true` means the
 * name is in the server-side built-in whitelist and cannot
 * be deleted or re-registered.
 */
export interface AgentDetail {
  name: string;
  description: string;
  kind: string;
  version: string;
  instructions: string;
  capabilities: string[];
  tools: string[];
  handoffs: string[];
  modelHint?: string;
  owner?: string;
  domain?: string;
  persona?: string;
  protected: boolean;
}

/** Body sent to `POST /api/v1/agents`. Mirrors the backend
 *  `AgentDescriptorRequest` struct. */
export interface AgentDescriptorRequest {
  name: string;
  description: string;
  kind: string;
  version?: string;
  instructions?: string;
  capabilities?: string[];
  tools?: string[];
  handoffs?: string[];
  modelHint?: string;
  owner?: string;
  domain?: string;
  persona?: string;
}

/**
 * One hit from `GET /api/v1/search` — the unified cross-domain
 * search over tool / mcp / skill / memory / agent / session
 * catalogs. `domain` names the catalog the hit came from;
 * `preview` carries a content snippet when the domain has one
 * (memory text, session transcript excerpt).
 */
export interface SearchHit {
  domain: string;
  id: string;
  title: string;
  score: number;
  preview?: string;
}

/** The search domains the backend registers, for filters and
 *  result badges. */
export const SEARCH_DOMAINS = ['tool', 'mcp', 'skill', 'memory', 'agent', 'session'] as const;

/** `GET /api/v1/sessions/:id/status` — the typed live-run snapshot
 *  the session controller publishes (`OperationSnapshot`). Only the
 *  fields the UI renders are typed; the backend also sends usage
 *  buckets and context-pressure projections we currently ignore. */
export interface OperationSnapshot {
  session_id: string;
  agent_name: string;
  state: string;
  iteration: number;
  max_iterations: number;
  taken_at: string;
  usage: { input_tokens?: number; output_tokens?: number } & Record<string, unknown>;
  last_error?: string | null;
}

/** Body for `POST /api/v1/skills` — the raw SKILL.md file content
 *  (frontmatter + markdown body) written to
 *  `<workspace>/.agents/skills/<name>/SKILL.md`. */
export interface CreateSkillRequest {
  name: string;
  content: string;
}

/** Response from `POST /api/v1/skills`. */
export interface CreateSkillResponse {
  name: string;
  path: string;
}

/** Response from `POST /api/v1/skills/reload`. */
export interface ReloadSkillsResponse {
  count: number;
}

/** Body for `POST /api/v1/tools` — registers a passthrough tool
 *  that echoes its arguments back. `input_schema` is the JSON
 *  Schema the model sees for tool calls. */
export interface RegisterToolRequest {
  name: string;
  description: string;
  input_schema: Record<string, unknown>;
}

/** Response of `POST /api/v1/sessions/{id}/fork`: the branch. */
export interface ForkSessionResponse {
  /** The new session — an ordinary conversation from here on. */
  session_id: string;
  /** The session it was branched from. */
  forked_from: string;
  /** Durable rows the child inherited. */
  rows: number;
}

export interface SessionSummary {
  id: string;
  status: string;
  context_id?: string;
  created_at?: string;
  /** Set when this session was opened by a delegated sub-agent
   *  (the `task` tool): the session the delegation ran in. A
   *  first-class session either way — provenance, not privilege. */
  parent_session_id?: string;
  /** The peer agent that ran a sub-agent session. */
  agent_name?: string;
}

/**
 * Full session detail returned by `GET /api/v1/sessions/:id`.
 *
 * `history` and `artifacts` use minimal local interfaces rather
 * than a third-party SDK — the REST endpoint returns plain JSON,
 * and we only need a few fields for display. `artifacts` is kept
 * as an empty array in practice; tool calls / results now flow
 * through `history` as agent frames.
 */
export interface SessionDetail {
  id: string;
  status: string;
  context_id: string;
  created_at?: string | null;
  updated_at?: string | null;
  history: SessionTurn[];
  artifacts: SessionArtifact[];
  /** Sub-agent provenance — see {@link SessionSummary}. */
  parent_session_id?: string;
  agent_name?: string;
}

/**
 * One durable event in the session transcript (one JSONL line).
 *
 * The session sink persists three durable families (see the
 * `event-durability-classification` spec — ephemeral system
 * events like `SessionStarted` / `SessionEnded` are broadcast
 * only and never persisted):
 *
 * - `{ "type": "UserInput", "data": { "text": string } }` —
 *   the synthetic envelope the controller writes for each
 *   user prompt.
 * - `{ "type": "Model", "data": ContentPart }` — one durable
 *   `AgentEvent::Model` frame; `ContentPart` is internally
 *   tagged on `type`: `text` | `tool_use` | `tool_result` |
 *   `resource`.
 * - `{ "type": "Agent", "data": [AgentMeta, inner AgentEvent] }`
 *   — a delegated sub-agent trace. Durability delegates to the
 *   inner event, so a child's model text / tool frames persist
 *   verbatim inside the recursive envelope. `AgentMeta` carries
 *   `parent_session_id` / `child_session_id` (a per-run trace
 *   id) / `parent_depth`; `data` may nest further `Agent`
 *   envelopes for grandchildren.
 * - `{ "type": "assistant_message", "data": Message }` — one
 *   assistant turn: the provider `Message` with the turn's text
 *   and its tool calls together.
 * - `{ "type": "tool_result", "data": Message }` — one tool
 *   outcome: the tool's output plus the id of the call it
 *   answers.
 */
export interface SessionTurn {
  type?: string;
  data?: Record<string, unknown> | unknown[];
  /** RFC 3339 timestamp of the persisted event, when present. */
  ts?: string;
  /** The durable ordinal the sink stamped on the row. `follow`
   *  `row` frames and the baseline's window carry it; the
   *  session-detail projection may omit it. It is the client's
   *  only cursor, and the number a resume token's `last_seq`
   *  points at. */
  seq?: number;
  /** Provenance the writer recorded beside the row, when it is
   *  not an ordinary turn boundary. Today only `"steer"` — a
   *  message the user injected into an already-live run. */
  source?: string;
  /** True when a `tool_result` row records a failed tool. The
   *  row's `data` is the tool *message* — the tool's output and
   *  the call's id — and the message carries no failure flag of
   *  its own, so the flag rides beside it. A row without it is a
   *  tool that returned. */
  is_error?: boolean;
  /** Surface op the server's fold applies to this row, when the
   *  row replaces a span of earlier rows instead of appending.
   *  A rerun's prompt row (regenerate) carries this so every
   *  projection — the server's `fold_log_surface` consumers AND
   *  this client-side reconstruction — shows the replacement
   *  story instead of the turn twice. Only `source_event_seqs`
   *  is consumed here; the server-side fold also validates
   *  `start` / `end`. */
  surface_op?: {
    start?: number;
    end?: number;
    source_event_seqs: number[];
  };
}

/** Legacy attachment slot — kept as an empty array in modern
 *  sessions so the detail page can iterate without
 *  special-casing undefined. */
export interface SessionArtifact {
  attachmentId?: string;
  name?: string;
  description?: string;
  parts?: ReadonlyArray<SessionPart>;
  /**
   * Server-side metadata on a legacy attachment. The modern
   * equivalent carries tool calls / results as agent turns
   * in `SessionDetail.history`; this metadata block is only
   * populated for sessions completed before history
   * persistence landed. We keep reading it for the legacy
   * fallback path.
   */
  metadata?: {
    kind?: string;
    tool_name?: string;
    tool_use_id?: string;
    is_error?: boolean;
    [key: string]: unknown;
  };
}

/**
 * Minimal view of one transcript segment.
 *
 * The v1 server uses field-presence serialization: exactly one of
 * `text` / `raw` / `url` / `data` is present per part. We type
 * only the `text` field because that's what the UI renders; other
 * content kinds fall through to a JSON dump.
 */
export interface SessionPart {
  text?: string;
  data?: unknown;
  raw?: string;
  url?: string;
  filename?: string;
  mediaType?: string;
}

/* ----------------------------------------------------------------
 * Session follow / approval / compaction (`routes/follow.rs`)
 * ----------------------------------------------------------------
 *
 * `GET /api/v1/sessions/{id}/follow` replaces the old
 * `/messages/stream` replay: its first frame is a complete baseline
 * (`FollowSnapshot`) that the client swaps its whole window for, and
 * later frames are increments — durable (`row`), transient (`live`)
 * or the open-interrupt set (`interrupts`). A reconnect therefore
 * never splices a local cache onto a server stream; it just replaces.
 */

/** What the session is doing, as `follow` reports it. */
export type SessionRuntimeState = 'idle' | 'running' | 'awaiting_approval' | 'cancelled';

/** Session-level facts carried by the `follow` baseline. */
export interface FollowHeader {
  session_id: string;
  user_id: string;
  agent_name?: string;
  state: SessionRuntimeState;
  created_at?: string;
  updated_at?: string;
}

/** How much of the model's window the next request would occupy. */
export interface SessionContextMeter {
  used: number;
  window?: number;
}

/** One tool call the in-progress turn proposed. */
export interface LiveToolCall {
  call_id: string;
  name: string;
  arguments: unknown;
  status: 'running' | 'completed' | 'failed';
  output?: string;
}

/** The transient projection of the turn that is live right now. */
export interface LiveTurn {
  turn_id: string;
  live_seq: number;
  running: boolean;
  text: string;
  reasoning: string;
  tool_calls: LiveToolCall[];
}

/** A high-risk call a suspended run is waiting on. */
export interface ApprovalRequest {
  id: string;
  call_id: string;
  tool: string;
  arguments_hash: string;
  risk: 'low' | 'high' | 'unknown';
  agent: string;
}

/** The complete first frame of a `follow` stream. */
export interface FollowSnapshot {
  header: FollowHeader;
  /** The last durable rows (a fixed window; `?from=` never widens it). */
  rows: SessionTurn[];
  /** Ordinal of the last durable row (`0` for an empty log). */
  cursor: number;
  live: LiveTurn;
  context: SessionContextMeter;
  pending_interrupts: ApprovalRequest[];
  /** Fingerprint of the log's newest request envelope (`''` when none). */
  request_snapshot_hash: string;
}

/**
 * The token a suspension issued, rebuilt from the baseline: the
 * log tail it ended at, the calls still waiting, and the request
 * envelope it was suspended under.
 */
export interface ResumeToken {
  last_seq: number;
  pending: string[];
  request_snapshot_hash: string;
}

/** What an operator decided about one held call. */
export type ResumeStatus = 'resolved' | 'cancelled' | 'rejected';

/** One decision in a resume request. */
export interface ResumeDecision {
  /** The interrupt (`ApprovalRequest.id`). */
  id: string;
  status: ResumeStatus;
  /** Denial reason, grant scope, … */
  payload?: unknown;
}

/** Session-scoped token usage and context occupancy. */
export interface SessionUsage {
  session_id: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  turns: number;
  context_used: number;
  context_window?: number;
}

/** How a submitted message reached its run. */
export type SteerDelivery = 'queue' | 'steer';

/** Response of `POST /api/v1/chat/sessions/{id}/messages`. */
export interface SendChatMessageResponse {
  message_id: string;
  /** `true` when the turn waits for the session's next run. A queued
   *  turn is **on disk** when this answer arrives: the server parks
   *  it as a durable `parked_prompt` row and flushes it before it
   *  answers, so the answer promises what the log holds. */
  queued: boolean;
  /** The delivery that actually happened — `queue` when no run was
   *  live to steer. */
  delivery: SteerDelivery;
}

/** What a manual compaction pass did. */
export interface CompactResult {
  committed: boolean;
  /** First and one-past-last shadowed surface positions. */
  start?: number;
  end?: number;
  shadowed_messages: number;
  shadowed_tokens: number;
  summary?: string;
  first_seq?: number;
  last_seq?: number;
  /** Why nothing was committed (`under_budget`, `empty_span`). */
  reason?: string;
}

/* ----------------------------------------------------------------
 * Schedules (`elysper-scheduler`)
 * ----------------------------------------------------------------
 *
 * The schedule domain is the `elysper-scheduler` crate
 * (`ScheduleStore` + `Scheduler`). Today the server does **not**
 * expose HTTP routes for it — see `SERVER-ROUTES-TODO.md` — but
 * the frontend pages commit to the wire shape so when the
 * backend is wired the pages need no rewrites. The shapes here
 * mirror `crates/elysper-scheduler/src/job.rs::Job` /
 * `JobKind` / `JobStatus`, snake_case kept as-is on the wire.
 *
 * `JobKind` is internally tagged on `kind`: `"cron"` carries
 * `expr`; `"interval"` carries `interval_secs`; `"once"`
 * carries nothing. The `interval_secs` is the interval as
 * seconds (uint) for `Interval` jobs; `cron` jobs leave it
 * undefined.
 */

export type JobStatus = 'active' | 'paused' | 'completed';

export type JobKind =
  { kind: 'once' } | { kind: 'interval'; interval_secs: number } | { kind: 'cron'; expr: string };

/// Minimal row returned by `GET /api/v1/schedules`. Backend
/// projects a tight shape so the list payload is small.
export interface ScheduleSummary {
  id: string;
  name: string;
  kind: JobKind;
  status: JobStatus;
  next_fire_at: string;
}

/// Full `Job` returned by `GET /api/v1/schedules/{id}`. The
/// backend detail endpoint flattens the Rust `Job` struct, so
/// every field lands at the top level.
export interface Schedule {
  id: string;
  name: string;
  description: string;
  kind: JobKind;
  payload: Record<string, unknown>;
  next_fire_at: string;
  last_fired_at: string | null;
  status: JobStatus;
}

/** Body for `POST /api/v1/schedules` — create a new schedule. */
export interface CreateScheduleRequest {
  name: string;
  description?: string;
  kind: JobKind;
  payload: Record<string, unknown>;
  /** Optional override for the first fire; if absent the server
   *  computes it from the kind (immediate for interval-with-zero,
   *  first cron occurrence for cron jobs, now for once). */
  next_fire_at?: string;
}

/** Body for `PATCH /api/v1/schedules/{id}` — pause / resume / update payload. */
export interface UpdateScheduleRequest {
  status?: JobStatus;
  payload?: Record<string, unknown>;
}

/** Tick result — what fired in the most recent scheduler pass. */
export interface ScheduleTick {
  fired: Array<{
    id: string;
    name: string;
    payload: Record<string, unknown>;
    fired_at: string;
  }>;
  next_deadline?: string | null;
}

/* ----------------------------------------------------------------
 * Workflows (`elysper-workflow`)
 * ----------------------------------------------------------------
 *
 * The workflow domain is the `elysper-workflow` crate. `WorkflowSpec`
 * is the document model: ordered `Step`s, optionally grouped into
 * `Phase`s for reporting. Five step kinds cover the orchestration
 * the runtime understands. Wire shapes mirror `crates/elysper-workflow/src/spec.rs`.
 */

export interface WorkflowSpec {
  /** Stable id the host uses to journal prefix replays. */
  id: string;
  steps: WorkflowStep[];
  phases?: WorkflowPhase[];
}

export type WorkflowStep = AgentStep | FanOutStep | BestOfStep | PipelineStep | MctsStep;

interface StepBase {
  /** Unique id within the document; the runtime keys journal entries
   *  on `(position, step_id)` so a doc-rewrite keeps replay intact. */
  id: string;
  /** Run inside an isolated worktree per call. */
  isolation?: boolean;
}

export interface AgentStep extends StepBase {
  kind: 'agent';
  agent: string;
  prompt: string;
  /** Command that must pass for the call to count as a success. */
  gate?: WorkflowGateRef;
}

export interface FanOutStep extends StepBase {
  kind: 'fan_out';
  agent: string;
  /** One prompt per item, all run concurrently under the same agent. */
  items: string[];
  /** Cap on this fan-out's own concurrency, on top of the run's. */
  concurrency?: number;
  /** Cap on `items` for this step; falls back to `WorkflowCaps::max_items`. */
  max_items?: number;
}

export interface BestOfStep extends StepBase {
  kind: 'best_of';
  agent: string;
  /** One candidate per prompt (the host's selection picks the winner). */
  items: string[];
  /** Cap on `items` for this step; falls back to `WorkflowCaps::max_items`. */
  max_items?: number;
  /** Cap on this step's own concurrency, on top of the run's. */
  concurrency?: number;
  /** Command every candidate has to pass to count as a success. */
  gate?: WorkflowGateRef;
}

export interface PipelineStep extends StepBase {
  kind: 'pipeline';
  /** Stages in order; each stage is itself a `WorkflowStep`
   *  (recursion matches the Rust `Vec<Step>` shape — a stage may be
   *  any step variant, including another pipeline or an mcts). */
  stages: WorkflowStep[];
}

export type MctsScorer =
  { kind: 'shortest_text' } | { kind: 'longest_text' } | { kind: 'heuristic' };

export interface MctsStep extends StepBase {
  kind: 'mcts';
  agent: string;
  /** The depth-0 prompt every branch starts from. */
  prompt: string;
  /** Number of parallel branches (Rust: u32, must be > 0). */
  branches: number;
  /** Number of *additional* depths after depth 0; `0` = single round. */
  max_depth: number;
  /** Cap on this step's own concurrency, on top of the run's. */
  concurrency?: number;
  /** Command every branch has to pass to count as a success. */
  gate?: WorkflowGateRef;
  /** Scorer the runtime ranks branches by. Required — the wire form
   *  matches `MctsScorer` (internally tagged on `kind`). A document
   *  naming `heuristic` requires the host to register the scorer on
   *  the runtime before the run starts. */
  scorer: MctsScorer;
}

export interface WorkflowPhase {
  /** Phase id; unique across the document (used as the live-control
   *  reference and as the prefix-replay lookup key). */
  id: string;
  /** Human-readable label, copied onto the calls it groups. */
  label: string;
  /** Step ids in this phase. A step may belong to at most one phase. */
  steps: string[];
}

/** Shell gate an agent call has to pass to count as a success. */
export interface WorkflowGateRef {
  command: string;
  /** Optional display label; the command is used when absent. */
  label?: string;
}

/** Body for `POST /api/v1/workflows` — create a workflow document. */
export interface CreateWorkflowRequest {
  /** Document JSON. Server validates by `WorkflowSpec::from_json`. */
  spec: WorkflowSpec;
}

/** How one planned call settled. Matches `CallStatus` in
 *  `crates/elysper-workflow/src/result.rs`. Default serde
 *  serialization (unit variants → bare string). */
export type CallStatus = 'succeeded' | 'replayed' | 'failed' | 'superseded' | 'skipped' | 'aborted';

/** How the step's gate treated the call. Matches
 *  `GateVerdict` in `crates/elysper-workflow/src/host.rs`. */
export type GateVerdict = 'absent' | 'passed' | 'failed' | 'unobserved';

/** Branch score attached to an MCTS call. Matches
 *  `BranchScore` in `crates/elysper-workflow/src/result.rs`. */
export interface BranchScore {
  branch_id: number;
  branch_index: number;
  text?: string | null;
  score?: number | null;
  gate: GateVerdict;
  winner: boolean;
}

/** One entry per planned call, in position order. Mirrors
 *  `CallRun` in `crates/elysper-workflow/src/result.rs`. The
 *  backend does not serialise the per-call prompt or agent
 *  name — those live on the parent step in the spec — so this
 *  type only carries what `result.rs` actually emits.
 */
export interface CallRun {
  position: number;
  step_id: string;
  phase?: string | null;
  status: CallStatus;
  text?: string | null;
  error?: string | null;
  gate: GateVerdict;
  winner: boolean;
  branch_score?: BranchScore | null;
}

/** Final result of one workflow run. Mirrors `WorkflowRun`
 *  in `crates/elysper-workflow/src/result.rs`.
 *
 *  The runtime is synchronous from the caller's perspective —
 *  a run is one `Result<WorkflowRun, WorkflowError>`, not an SSE
 *  stream. There is no `WorkflowRunEvent` wire type.
 *
 *  Wire-status caveat: `WorkflowRun` and its supporting types
 *  (`CallRun` / `BranchScore` / `CallStatus` / `GateVerdict`)
 *  are in-process only today — `result.rs` does not derive
 *  `Serialize` / `Deserialize`. When the backend adds the
 *  `#[derive(Serialize)]` + `#[serde(rename_all = "snake_case")]`
 *  pair to expose `/api/v1/workflows/{id}/run`, this shape is
 *  the projected wire format. Snake_case is assumed per the
 *  project convention already used by `Phase` in `spec.rs`. */
export interface WorkflowRunResult {
  /** Run id (`wf_` + ULID); every request the run made carries it. */
  run_id: string;
  /** Last top-level step's text output, when it produced one.
   *  For fan-out / pipeline: every non-empty text in position
   *  order, joined by a blank line. For best_of: the winner's
   *  text alone. */
  output?: string | null;
  /** One entry per planned call, in position order. */
  calls: CallRun[];
  /** How many calls came back from the journal instead of spawning. */
  replayed: number;
  /** How many calls crossed the host in this run. */
  spawned: number;
}

/* ----------------------------------------------------------------
 * Evals (`elysper-eval`)
 * ----------------------------------------------------------------
 *
 * Eval is an offline grading framework (`EvalSuite` / `TestCase` /
 * `Metric` / `EvalRunner`). The frontend commits to the wire shape
 * so a future `/api/v1/evals/*` set needs no rewrites here.
 *
 * `Metric` is the configuration side — keyword, judge, or schema
 * validation. The runner fans out to `EvalAgent` to produce the
 * actual outputs, then scores.
 */

export interface EvalSuite {
  name: string;
  cases: EvalTestCase[];
}

export interface EvalTestCase {
  id: string;
  input: string;
  /** Required substrings the keyword metric checks for. */
  expected_keywords?: string[];
  /** Exact-output override; checked verbatim when set. */
  expected_output?: string;
  /** Test-case-specific metric config; suite metric is the fallback. */
  metrics?: EvalMetricConfig[];
}

export type EvalMetricConfig =
  | { kind: 'keyword_match' }
  | { kind: 'llm_judge'; provider: string; criteria: string }
  | { kind: 'schema_validation'; schema: Record<string, unknown> };

/** Body for `POST /api/v1/evals` — register a suite from JSON. */
export interface CreateEvalSuiteRequest {
  suite: EvalSuite;
}

/**
 * `EvalReport` — wire shape mirrors
 * `crates/elysper-eval/src/lib.rs::EvalReport`. The backend
 * serialises `generated_at: DateTime<Utc>` as RFC 3339 by
 * default, so this is `string` on the wire.
 */
export interface EvalReport {
  suite_name: string;
  /** RFC 3339 instant when the runner finished. */
  generated_at: string;
  results: EvalTestResult[];
  /** Mean of every per-case × per-metric score in this report. */
  average_score: number;
  passed: number;
  total: number;
}

/**
 * `EvalTestResult` — mirrors
 * `crates/elysper-eval/src/lib.rs::TestResult`. The runner
 * emits per-metric scores in a sorted map so the JSON is
 * deterministic.
 */
export interface EvalTestResult {
  case_id: string;
  /** What the agent under test produced. */
  actual_output: string;
  /** Per-metric scores in `[0.0, 1.0]`. */
  scores: Record<string, number>;
  passed: boolean;
}

/* ----------------------------------------------------------------
 * Tasks (`elysper-tool-task`)
 * ----------------------------------------------------------------
 *
 * The `task` tool is the multi-agent subagent delegation seam. It
 * is **model-facing** today — agents call it during a chat run —
 * so the "task history" surfaced in the UI is reconstructed from
 * session events with `{ "type": "Agent", "data": [AgentMeta, …] }`
 * envelopes. The frontend types below describe what we extract
 * from those envelopes; there is no `/api/v1/tasks` endpoint to
 * read from independently.
 */

export interface TaskDelegation {
  /** Synthetic id `<parentSessionId>:<childSessionId>` — names one
   *  delegation for good, so the URL `/tasks/:id` survives a
   *  refresh. */
  id: string;
  /** Session that produced this delegation. */
  parent_session_id: string;
  /** Per-run trace id the subagent's session mirrors. */
  child_session_id?: string;
  /** Depth from the root session (root = 0). */
  parent_depth: number;
  /** Agent name of the delegated subagent. */
  agent: string;
  /** The prompt the parent sent the subagent. */
  prompt: string;
  /** Subagent's resulting text output, if any. */
  output?: string;
  /** RFC 3339 instant the delegation started. */
  started_at?: string;
  /** RFC 3339 instant the delegation finished. */
  finished_at?: string;
  /** Status mirrored from the inner event. */
  status: 'pending' | 'running' | 'succeeded' | 'failed' | 'aborted';
}
