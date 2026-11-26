# The frozen public surface

This page is a **commitment**, not a snapshot. It names every item that
the seven library crates promise to keep importable, says how long the
promise runs, and describes the one process by which the list may
change. A machine-readable copy of the same list lives in
[`scripts/frozen-api/baseline`](../scripts/frozen-api/baseline). The gate
`make check-frozen-api` compares the **live curated surface** against
that baseline and fails the build whenever they disagree — in either
direction. It does not read this page: keeping this page and the
baseline in step is the review convention the change procedure below
describes, not something the script can check.

## What is frozen

Each library crate decides its public surface in exactly one place: the
explicit `pub use` lists at the top of its `src/lib.rs`. A glob
re-export publishes an unbounded set of names, so a frozen crate may
not have one: `make check-frozen-api` refuses every glob form in a
frozen crate (`pub use module::*;` as well as `pub use module::{a, *};`)
and names the crate and the glob it found. The repo-wide style ban is
a different, weaker rule: `make check-pub-surface` matches only the
single-segment lowercase form (`pub use foo::*;`) and exempts the
facade, which propagates each crate's already-curated surface. So
every public name a frozen crate chose to export literally appears in
one of those lists — including names nested in sub-lists such as
`pub use agent::{..., host::{Gate, Hook, ...}, ...}`.

The lists are **crate-root** lists. A `pub use` inside a module — an
inline `#[cfg(test)] mod tests { ... }`, a `pub mod foo { ... }` — names
something that module re-exports, not something the crate root
publishes, so it is not on this page and not in the baseline.

The frozen surface is that set of names, per crate:

| Crate | Frozen names |
|---|---|
| `uva-core` | 41 |
| `uva-context` | 19 |
| `uva-harness` | 59 |
| `uva-provider` | 93 |
| `uva-session` | 156 |
| `uva-steering` | 36 |
| `uva-tool` | 45 |

The full lists are in [The frozen names](#the-frozen-names) below. Every
entry is spelled exactly as you would write it after the crate name:
`use uva_harness::{ReActAgent, Agent}`.

A handful of these names exist only behind a Cargo feature or platform
flag (`OpenAICompatibleProvider` needs `openai`, the `AnthropicStream*`
types need `anthropic`, `signal_of` is unix-only). They are on the list
under the same rule: the *name* stays on the curated surface; the code
behind it still obeys its feature flag.

## What is *not* frozen

Be explicit about the other side of the promise:

- **Deep paths.** Anything reachable only by a longer path — for
  example `uva_harness::agent::host` internals, or any `pub` item
  inside a module that lib.rs did not re-export — is **not** covered.
  Those names can move or disappear in any release without a major
  announcement. If you depend on one, tell us, and we will consider
  curating it.
- **Items defined in lib.rs outside the `pub use` lists.** A few lib.rs
  files define public items directly (e.g. the `Result` type alias in
  `uva-core`). Those are not part of the curated lists and are not
  covered by this promise. (Where a name *is* re-exported — as
  `uva-tool` re-exports `Result` from its `types` module — it is
  frozen like any other.)
- **Crate-root `pub mod` names.** `uva_core::agent` (the module
  itself) is not on the curated list — only the names the crate root
  re-exports from it. A module path can move without a major bump.
- **The crates this page does not list.** `uva-core-tokio` sits
  alongside the frozen seven without being frozen: its surfaces may
  still move before `0.2`. So is the
  facade (`uva`), which only propagates the surfaces below.
- **The server's HTTP API.** `/api/v1` URLs version themselves; this
  page covers only the library crates listed above.
- **Signatures.** The promise is about the *set of names*: each listed
  name stays importable from the crate root, spelled exactly as listed.
  The shapes behind the names (parameters, generics, trait bounds)
  follow ordinary pre-1.0 evolution, recorded in the changelog.

## Recorded shape changes

Signatures evolve under the rule above. The breaking shapes this line
ships are recorded here, beside the promise they narrow:

- **The session's durable goal joined the curated surface** (the goal
  tool). `goal` joined the curated `uva-session` surface: the typed
  builder for the `Goal` row — the one row per change that carries a
  session's objective, its state and its token budget. No name left
  the surface, so the `uva-session` count moves 155 → 156. Migration:
  build the row with `goal(state, objective, budget_tokens)` and
  append it with `append_event`; the tool itself
  (`uva_tool_plugins::goal`) is what a deployment mounts.

- **The streaming call returns the fold's turn** (stream fold, ADR 0029).
  `ModelProvider::complete_with_stream` returns the fold's
  `SamplingResult` rather than a `CompletionResponse`, and
  `SamplingResult::into_completion_response` is **gone**: a
  non-streaming response is now produced only by `complete`.
  `StreamChunk` gained two variants — `ReasoningSignature` and
  `ToolCallName` — and its `Stop` variant is the terminal fact of a stream.
  No crate-root name moved —
  `CompletionResponse`, `SamplingResult`, `StreamChunk` and
  `BlockAssembler` all stay importable from the crate root — so the
  frozen baseline is unchanged. Migration: a caller that wants a
  `CompletionResponse` from a stream must fold the chunks with
  `BlockAssembler` itself, which is what the harness and both adapters
  already do.

- **`Host` lost its interception registration surface** (interception
  table, ticket 04 — ADR 0028). `Host::{hook, gate,
  request_transformer, result_transformer, prompt_section}` are **gone**:
  the four interception traits and prompt sections now register on the
  deployment's one `uva_hook::HookRegistry`, and the loop dispatches
  that single table. The packaging bundle built on those methods left
  with them — `uva_harness::agent::host::Plugin` (also re-exported as
  `uva_harness::Plugin`), `Host::install` and
  `uva_harness::PluginHandle` are **no longer importable** — and
  `Host::default_policy` now takes the table it writes into:
  `default_policy(workspace, registry)`. `Host` keeps the typed tables
  the registry cannot express (`guard`, `hint`, `tracker`,
  `output_transformer`, `tool`), and `Host::hook_registry()` is the
  loop's read handle on the table. The *names* `Host`, `Hook`, `Gate`,
  `RequestTransformer`, `ResultTransformer`, `HookContext`,
  `Arbitration` and `Registration` stay on the frozen list and their
  import paths do not move; `Plugin` and `PluginHandle` leave it, so the
  `uva-harness` count moves 61 → 59. Migration: build one
  `HookRegistry` at boot (or read the one a host carries), register
  with `register_hook_at` / `register_gate_at` /
  `register_request_transformer_at` / `register_result_transformer_at`
  / `register_prompt_section` under a `MountPoint` and a
  `RegistrationOrigin`, and drop the returned `HookRegistration` to
  withdraw it. The five observation points (`before_run`, `after_run`,
  `before_provider_call`, `on_error`, `on_compaction`) are now
  registrable from an out-of-tree crate that depends only on
  `uva-hook`.

- **Three names left the curated surface** (plugin-crate splits, ticket
  10 — the delegation channel's retirement). `DelegatingTool` (the
  delegating-tool trait), `DelegationCall` (the call payload it
  received) and `AgentMeta` (the sub-agent metadata the wrapped
  `AgentEvent::Agent` variant carried) are **no longer importable**
  from `uva-harness` — the delegation seam is gone with the channel
  itself. The delegation capability did not disappear with the names:
  the `task` tool is an ordinary registry tool in `uva-tool-task`, and
  it runs a peer agent as a child run without any of the three types.
  The event vocabulary lost the `AgentEvent::Agent` wrapper variant
  with them, so a child's events stay in the child's own session
  stream instead of travelling into the parent's as a wrapped frame.
  Migration: install `task` through the tool registry (see
  `uva-tool-task`'s README) and read the child's transcript from the
  child session.

- **`SubagentWriters::open_child` changed shape** (plugin-crate splits,
  ticket 10). The method took one `&AgentMeta` argument; it now takes
  the facts it needs as named arguments — `child_session_id`,
  `parent_session_id`, `depth`, `agent_name` and `prompt` — because
  the metadata struct left with the wrapper variant. The *name*
  `SubagentWriters` stays on the frozen list and its import paths do
  not move.

- **`HookContext` lost its `events` field** (plugin-crate splits, ADR
  0004). The context a `Hook`, `Gate`, `RequestTransformer` or
  `ResultTransformer` sees now carries the run state (`state`) and the
  cancellation token (`cancel`) only — the sink half is no longer part
  of the contract, because every out-of-tree registration would have
  had to name a host-private type. The loop passes its sink explicitly
  where a dispatch writes the durable `hook_block` veto row (the
  `Seams` dispatch methods and `Host::dispatch_hooks` gained that
  argument). The *name* `HookContext` stays on the frozen list and its
  import paths do not move: `uva-hook` owns the type and `uva-harness`
  re-exports it from both the crate root and `agent::host`.

- **`uva_tool::Context` gained `cancel`** (plugin-crate splits, ticket
  08). Every tool dispatch now carries the run's cancellation token
  (`Arc<dyn CancelToken>`) beside the session id, workspace root and
  output bound; the loop installs the run's own token before dispatching
  a call, and a tool that starts nested work (the `task` tool driving a
  child run) hands it down instead of inventing a second cancellation
  channel. `Context::new` / `Context::default` still build a context
  whose token is never cancelled; `Context::with_cancel` is the chained
  injection point. The *name* `Context` stays on the frozen list and its
  import paths do not move.

- **`Arbitration` moved to `uva-hook`** (hook registry + gate, ADR
  0004). The type now lives in the contract crate beside the
  `HookRegistry` whose dispatch produces it; `uva-harness` re-exports
  it from both the crate root and `agent::host`, so the frozen name
  and its import paths do not move. Its shape is unchanged (`decision`
  + `asks`).


- **`DiagnosticKind` lost `UnsupportedDecision` and `GateFailed`;
  `Host::accept` removed** (interception arbitration, ADR 0020). The
  arbitration's facts — a gate that could not answer, a decision the
  point did not accept — now have one shape and one owner
  (`uva_hook::arbitration::Fact`) and land in the run's durable record
  as `hook` warnings, so the host's diagnostic ring keeps only the
  host's own facts. The *names* `DiagnosticKind` / `Diagnostic` /
  `Host` stay on the frozen list and their import paths do not move.

- **The row vocabulary took the session log's shape** (row vocabulary,
  ADR 0006 — the one reviewed exception to the no-removal rule, see
  *How long the promise runs*). Four names moved in the frozen
  `uva-session` surface:

  - `KNOWN_SESSION_EVENT_TYPES` is **gone**. The list had no production
    caller, and its documentation claimed both that the wire contract
    derived from it and that the persistence layer rejected unknown
    types — neither was true. The tags are published by `RowTag`'s
    schema now (a `oneOf` of consts), which is what the wire contract
    and the generated frontend union read. Migration: enumerate
    `schemars::schema_for!(uva_session::RowTag)`, or ask
    `RowTag::from_wire` whether a spelling is a tag.
  - `empty_event_of` is **gone**. It had no production caller either.
    `RowTag::empty_event` is the tag-side constructor and
    `SessionEvent::tag` is the event-side answer; a test chains the two,
    so neither match can drift a pairing. Migration:
    `RowTag::empty_event(tag)`.
  - `SessionEvent::LifecycleShutdown` is now
    `SessionEvent::SessionShutdown`. **The wire bytes do not change**
    (`{"type": "session_shutdown", …}`): the enum derives its spelling
    from `rename_all = "snake_case"`, and the explicit rename the
    variant used to carry is gone. Only Rust source that named the
    variant moves.
  - `LogWriter::append` and `LogWriter::append_nowait` narrowed from
    `pub` to `pub(crate)`. A row's shape — its tag and payload — is no
    longer something an out-of-tree caller can spell. Migration: build
    the event with a typed constructor (`user_message`,
    `assistant_message`, …) and append it with `append_event` /
    `append_event_nowait`; `append_copied` remains the batch gate that
    copies an existing batch of rows into a log — their own `ts` kept,
    this log's ordinals stamped — so history round-trips without
    reopening the hand-written path.

  The `uva-session` count moved 133 → 134: `RowReading` joined the
  curated surface — one row's class and its decode outcome, read once.
  The two names above were already off the list when this entry was
  written, which is why they move the promise and not the count.

- **The row vocabulary now restores a row at the read boundary** (row
  vocabulary, ADR 0007 — the second reviewed exception to the
  no-removal rule, see *How long the promise runs*). Four names left
  the curated `uva-session` surface and five joined it:

  - `RowClass` is **gone**. A row's class was a runtime answer every
    read site had to branch on — and the branch was the defect: the
    `Legacy` arm was droppable, and one read site dropped it (a
    pre-typed log's assistant rows got no turn label). The vocabulary
    now restores a row to its **current form** at the read boundary,
    so a reader sees a `RowTag` and never a dialect. Migration:
    `classify_row(row)` → `restore_row(row).tag()`.
  - `RowReading` is **gone** — the restored row (`CurrentRow`) carries
    the same facts, and its `unreadable()` is now
    `defect().is_some() || event().is_none()`.
  - `carries` is **gone**. The tag↔dialect pairing is now *executed*
    by the restore rather than declared in a table — and the table was
    wrong for the role-polymorphic dialect: a legacy `Message`
    envelope's typed counterpart depends on the payload's role, so it
    has no single counterpart.
  - `classify_row` is **gone** — `restore_row` replaces it, and it
    also answers what a row *means*, not only what it is.

  `CurrentRow` (a row restored to the current form), `RowDefect` (why a
  row is not a current row), `UnreadableLog` (a log this build cannot
  interpret), `restore_row` (the restore itself) and `SurfaceRole` (the
  role the vocabulary declares) joined the curated surface. The
  `uva-session` count moved 134 → 135.

- **The row vocabulary now publishes the current row** (row
  vocabulary, ADR 0007's cross-language half). Two names joined the
  curated `uva-session` surface — `publish_row` (one row in the
  current shape: a current row as it is, a dialect row rewritten, a
  row with no current form not published at all) and `publish_rows`
  (a log's rows, in order) — and the count moved 135 → 137. The
  server's transcript routes publish through them, so a legacy
  dialect spelling never reaches the wire and the browser's dialect
  dispatch table retired. `RowDialect` stays public: the server must
  be able to *name* a legacy row to restore it; only its publication
  stopped.

- **The row's payload shape moved into its constructors** (row payload
  shape, ADR 0021). Two shapes changed on the curated `uva-session`
  surface:

  - `turn_end` gained a third parameter, the optional failure detail
    (`Option<&str>`): a failed turn's message is part of the row, so
    the constructor writes the payload and the typed fields in one
    place and no caller patches the payload after the fact. The
    *name* `turn_end` stays on the frozen list and its import path
    does not move.
  - `SessionEvent::plugin` — the associated constructor a plugin's
    own row used to be built through — is **gone**. Plugin rows are
    built by `plugin_row` instead, which joined the curated surface
    with the same signature and the same payload bounding. The *name*
    `SessionEvent` stays on the frozen list and its import path does
    not move.

  `compaction_start`, `compaction_summary`, `compaction_row`,
  `compaction_end_committed`, `compaction_end_failed` and `plugin_row`
  joined the curated surface (152 → 153 names), and `STEP_INTERRUPTED`
  — the `action` the crash repair writes into the step-end row it
  closes an interrupted turn with — joined it with this change
  (153 → 154), so an out-of-tree reader can match the value it reads
  off the wire by its crate-root name.

- **The server's wire types grew** (TUI wire shapes, ADR 0025). The
  **seven frozen crates are untouched by this line** — what follows is
  the *server* crate's public surface, recorded here because the
  terminal now reads these very types as its wire contract: the
  terminal keeps no copy of them, so the shape both ends decode is
  one shape.

  - **Added** — the `rpc::methods::names` module (64 `pub const`
    method names plus `pub const ALL`), and
    `rpc::notify::ApprovalRequestNotification` (with its `request()`
    accessor).
  - **Added (public methods)** — `routes::chat::Delivery::as_wire` and
    `routes::ui::UiSchemaResponse::components_in`.
  - **Visibility widened** — `routes::search::DEFAULT_LIMIT` is now
    `pub`: the terminal asks for the search route's own page size.
  - **Signature changed** — `routes::follow::FollowEvent::payload()`
    returns `Result<Value, serde_json::Error>` instead of `Value`, and
    lost its `#[must_use]`.
  - **Field type changed** — `session::controller::LiveToolCall::status`
    is a `String` instead of a `&'static str`.
  - **New `Deserialize` derives** — `routes::follow::{SessionHeader,
    ContextMeter, FollowSnapshot, FollowEvent}`,
    `routes::chat::{CreateSessionResponse, SendMessageResponse,
    WireAttachment}`, `routes::sessions::SessionSummary`,
    `session::status::SessionStatus`, `ui::schema::{UiSchema,
    UiComponent}` and `ui::tree::SlotInfo`.

  **The wire bytes do not change**: `LiveToolCall::status` is a
  Rust-side type change (`&'static str` → `String`) that serializes to
  the same string. What changed is whether a client can decode the
  frame back — that is the whole point of the line.

- **`uva_core::paginate_registry_list` takes the row's key**
  (list recipe, ADR 0027). The primitive used to require
  `T: RegistryItem` for one thing only — `item.name()`, the value the
  cursor encodes and resumes after. It now takes `id_of: impl Fn(&T)
  -> &str` and has no bound on `T`, so a list whose rows are wire types
  with no registry metadata (a session summary, a schedule row) uses
  the same slicing arithmetic instead of carrying a second copy. The
  two production registries pass `|entry| entry.name()`; the cursor,
  `next_cursor`, `total` and the deleted-cursor empty page are
  unchanged. The *name* `paginate_registry_list` stays on the frozen
  list, spelled and importable exactly as before; the `uva-core` count
  does not move. Adding the argument is a call-site edit at three
  places (the agent registry, the tool registry, one in-crate mock),
  all inside this workspace.

## How long the promise runs

The workspace is on the `0.1` line. **Every name listed below is
promised for the remainder of that line** — no removals, no renames, in
any `0.1.x` release. At the next minor bump (`0.2`) the commitment is
revisited: names may move, but only together with an updated version of
this page.

Cargo treats `0.y.z` versions as allowing breaking changes at any point;
this page deliberately narrows that for the curated surface. If the
maintainers ever need to break the promise early, that change must land
as a minor bump with the migration spelled out here — never silently
inside `0.1.x`.

The row-vocabulary block above is the **one reviewed exception** to
that rule, made under ADR 0006's decision that `uva-session` owns the
row vocabulary. Two of the names had no caller outside their crate, the
one rename leaves the bytes on the wire untouched, and the narrowed
writers close the only remaining hand-written row entry point. It is
recorded here, with its migrations, rather than absorbed silently; the
`0.1.x` promise stands for every other name on the list.

ADR 0007's block is the **second** reviewed exception, on the same
ground: `RowClass`, `RowReading`, `carries` and `classify_row` are the
runtime half of a vocabulary that ADR 0006 had already made the owner
of the row, and every one of them is replaced by the restore at the
read boundary. The migration for each is spelled out in the block
above. The `0.1.x` promise stands for every other name on the list.

## The gate

`make check-frozen-api` compares the live curated surface against
`scripts/frozen-api/baseline` (one `crate-name::Name` per line, sorted)
and fails on both kinds of drift:

```text
NEW:     uva-harness::ShinyNewThing      # a name the baseline never promised
REMOVED: uva-harness::ReActAgent         # a promised name that disappeared
```

A new public name failing the gate is the intended behaviour: it forces
the addition to be an explicit, reviewed decision. The gate is wired
into `make ci`, next to its two relatives:

- `check-pub-surface` bans the single-segment lowercase glob form
  (`pub use foo::*;`) outside the facade — a style rule, and a
  narrower one than this gate's;
- `check-pub-surface-strict` ratchets the *number* of `pub use`
  statements per crate;
- `check-frozen-api` (this gate) pins the *identity* of every exported
  name — a count that holds while `ReActAgent` quietly becomes
  `ReactAgent` holds nothing.

The gate runs its own fixtures first
(`scripts/check-frozen-api.sh --self-test`): the scope cases, the
glob refusal, and the diff itself. A gate nobody can test is a gate
whose behaviour is whatever the last reader remembers it to be.

## Changing the surface

One reviewed change, three files, together:

1. **Adding a name.** Put the `pub use` entry in the crate's `lib.rs`,
   regenerate the baseline
   (`scripts/check-frozen-api.sh --print > scripts/frozen-api/baseline`),
   and add the name to this page's list. The review is where "does this
   belong on the curated surface?" gets answered — new names are
   liabilities, not features.
2. **Removing or renaming a name.** Not within `0.1.x` — the gate fails,
   and that failure is correct. At a minor bump, the removal, the
   baseline, and this page change in one commit, with the migration
   written where users will see it (the changelog and this page).

If the baseline and this page ever disagree with the code, the gate is
red until a human realigns all three. The gate exists so that "just
this once, ship it" leaves a visible mark.

## The frozen names


Sorted per crate; the backticked spelling is the exact import name.


The lists below are generated from `scripts/frozen-api/baseline` —
if a list here disagrees with the baseline, the baseline wins and this
page is stale. Regenerate the baseline with
`bash scripts/check-frozen-api.sh --print > scripts/frozen-api/baseline`
and rewrite these lists in the same change.


### uva-core — 41 names

Runtime seams without a runtime: clock, cancellation, ids, sleep, plus
small shared registries and text helpers.


`AgentDescriptor`, `AgentFilter`, `AtomicCancelToken`, `Blocking`, `BreakerConfig`
`BreakerObservation`, `BreakerPhase`, `BreakerState`, `CancelToken`, `Clock`
`Error`, `FixedClock`, `FixedSleeper`, `FullOutputStore`, `IdGen`
`InMemoryFullOutputStore`, `MAX_LIMIT`, `NameTable`, `Outcome`, `Registry`
`RegistryItem`, `RegistryList`, `SchemaViolation`, `Sensitive`, `SensitiveData`
`SequenceGenerator`, `SharedClock`, `SharedIdGen`, `Sleeper`, `SystemClock`
`ThreadBlocking`, `ThreadSleeper`, `UlidGenerator`, `cap_to_char_boundary`, `paginate_registry_list`
`panic_message`, `parse_judge_score`, `redact_partial`, `redact_partial_with`, `truncate_chars`
`validate_against_schema`


### uva-context — 19 names

The context-window policy and the three-tier memory paradigm.


`AgentState`, `ContextManager`, `FileMemory`, `FileMemoryConfig`, `FileMemoryEntry`
`FileMemoryError`, `InMemoryMemory`, `KEY_MEMORY`, `MAX_MEMORY_INDEX_LINES`, `Memory`
`MemoryEntry`, `MemoryError`, `MemoryKind`, `MemoryPlugin`, `MemoryScope`
`NoopContextManager`, `SharedContextManager`, `TruncatingContextManager`, `UsageMeter`


### uva-harness — 59 names

The agent loop: strategies, approval/resume, checkpoints, the host's
typed policy tables and its one interception registry, events, and the
loop's own contract.


`Agent`, `AgentCaps`, `AgentDescriptor`, `AgentEntry`, `AgentEvent`
`AgentInput`, `AgentRegistry`, `AgentRuntime`, `ApprovalGate`
`ApprovalMode`, `ApprovalPlugin`, `ApprovalPolicy`, `ApprovalRequest`, `Arbitration`
`ChannelInbox`, `DEFAULT_MAX_ITERATIONS`, `DEFAULT_SYSTEM_PROMPT`
`Diagnostic`, `DiagnosticKind`, `EventSink`, `EventStream`, `Gate`
`GrantKey`, `GrantScope`, `Hook`, `HookContext`, `Host`
`KEY_APPROVAL`, `PromptContext`, `PromptSection`
`ReActAgent`, `ReActStrategy`, `ReasoningStrategy`, `Registration`, `RequestTransformer`
`ResultTransformer`, `ResumeCoverageError`, `ResumeDecision`, `ResumeStatus`, `ResumeToken`
`RiskOf`, `RunInbox`, `SessionEndReason`, `StrategyRegistry`, `SubagentWriters`
`Suspension`, `SystemEvent`, `WarningKind`, `align_decisions`, `check_coverage`
`default_descriptor`, `latest_request_snapshot_hash`, `may_retry`, `open_requests`, `prepare_messages_for_shot`
`single_shot_request`, `single_shot_request_shared`, `suspended_segment`, `workspace_hash`


### uva-provider — 93 names

Model providers and everything around a completion: request/response
types, credentials, retry classification, cache policy, JSON repair.


`AnthropicProfile`, `AnthropicProvider`, `AnthropicStreamContentBlock`, `AnthropicStreamDelta`, `AnthropicStreamEvent`
`BilledClass`, `BlockAssembler`, `CacheControlMark`, `CachePolicy`, `CachePolicyApplier`
`CacheScope`, `CacheTtl`, `CompletionRequest`, `CompletionResponse`, `Content`
`ContentPart`, `ContextOverflowDetector`, `CredentialError`, `CredentialStatus`, `DEFAULT_ANTHROPIC_CONTEXT_WINDOW`
`DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS`, `DEFAULT_OPENAI_CONTEXT_WINDOW`, `DEFAULT_OPENAI_MAX_OUTPUT_TOKENS`, `DEFAULT_STREAM_IDLE_TIMEOUT`, `EMPTY_RESPONSE_MARKER`
`IDLE_TIMEOUT_MARKER`, `ImageContent`, `ImageDetail`, `KEY_LLM`, `KEY_LLM_DEFAULT`
`MAX_PROVIDER_ERROR_BODY_CHARS`, `Message`, `MessageCacheStrategy`, `MessageKind`, `ModelAdapterPlugin`
`ModelCapabilities`, `ModelConfig`, `ModelInfo`, `ModelProvider`, `ModelTier`
`OpenAICompatibleProvider`, `OpenAIProfile`, `ProviderConfig`, `ProviderEntry`, `ProviderErrorBody`
`ProviderInfo`, `ProviderProfile`, `ProviderRegistry`, `ProviderResolver`, `ReasoningContent`
`ResourceLink`, `RetryClass`, `RetryConfig`, `Role`, `SamplingResult`
`StreamChunk`, `StubProfile`, `TextContent`, `TierLimits`, `TierParseError`
`TokenCounter`, `TokenUsage`, `ToolAnnotations`, `ToolArgsQuality`, `ToolChoice`
`ToolDefinition`, `ToolResult`, `ToolUse`, `WorkspaceConfig`, `apply_cache_policy`
`classify`, `classify_error`, `classify_provider_error_body`, `complete_partial_json`, `completion_to_sampling`
`empty_response_error`, `estimate_messages_token_count`, `idle_timeout_error`, `is_empty_response_error`, `is_idle_timeout`
`is_length_stop_reason`, `is_retryable_error`, `is_silent_overflow`, `normalize_api_key`, `parse_provider_error_body`
`parse_retry_after`, `parse_tool_input`, `parse_tool_input_logged`, `parse_tool_input_reported`, `parse_tool_input_with_completion`
`repair_json`, `retry_config_for`, `synthesize_orphan_result`


### uva-session — 156 names

The durable ledger: JSONL sink and version header, events, the row
vocabulary (tags, dialects, the restore to the current row, borrowed
reads), history fold, surface ledger, compaction, search, token
metering, typed builders.


AttachmentRefRow, BalanceCache, CURRENT_SCHEMA_VERSION, CompactionDetails, CompactionError
CompactionLifecycleOutcome, CompactionOutcome, CompactionPass, CompactionPassError, CompactionPlan
CompactionPlugin, CompactionPluginConfig, CompactionRef, CompactionSettings, Compactor
ContextPressure, CurrentRow, DEFAULT_BUS_CAPACITY, DEFAULT_LIMIT, DEFAULT_PRUNE_MIN_CHARS
DEFAULT_SUMMARY_TIMEOUT, EntryHit, FoldError, FoldedSurface, HistoryError
JsonlSessionSearch, KEY_COMPACTION, KEY_SESSIONS, LogWriter, MAX_PLUGIN_KIND_BYTES
MAX_PLUGIN_PAYLOAD_BYTES, MappingGap, MeasurementBaseline, MetaMarker, OperationSnapshot
OperationState, OrphanedCompaction, PRUNED_TOOL_RESULT, ParkedPromptRecovery, PlannedCompaction
ProvenancedHistory, RepairReport, ReplaceRange, ResolvedReplace, RowDefect
RowDialect, RowTag, Rows, SearchError, SearchQuery
SessionEndReason, SessionError, SessionEvent, SessionHit, SessionLog
SessionMetadataHeader, SessionSearch, SessionSink, SessionSnapshot, SessionStoreConfig
SessionStorePlugin, SharedSessionSearch, SharedSurfaceLedger, SnapshotBus, SnapshotReceiver
STEP_INTERRUPTED, SummaryCompactor, SummaryInput, SurfaceLedger, SurfaceOp, SurfaceRole
SurfaceToken, TOOL_NOT_STARTED, TOOL_OUTCOME_UNKNOWN, TURN_INTERRUPTED, TokenBudget
TokenMeasurement, TokenMeter, TokenMeterError, TouchedPair, UnbalancedCut
UnreadableLog, UsageBuckets, ValidationError, approval_asked, approval_decided
approval_granted, assemble_chunks, assistant_message, balanced_cut_at_or_before, balanced_message_cut
begin_compaction, build_balance_cache, compaction_balanced, compaction_end_committed, compaction_end_failed
compaction_outcomes, compaction_row, compaction_start, compaction_summary, derive_history
estimate_message_tokens, feedback, finish_compaction, fold_log_surface, fold_surface
goal, hook_block, interrupted_turn_closers, is_metadata_header_row, iteration_end
iteration_start
jsonl_session_search, log_tail_seq, message_row, modified_at, orphaned_compactions
park_consumed, parked_prompt, parked_prompts, plan_compaction, plan_compaction_from_rows
plugin_row, prune_tool_results, publish_row, publish_rows, repair_log
request_header, request_snapshot, restore_row, round_trip, session_shutdown
should_compact, step_end, step_end_suspended, step_start, structural_kind
subagent_enter, subagent_exit, surface_message, surface_to_messages, tool_dispatch
tool_output, tool_result, tool_result_transformed, touched_target, try_fold_log_surface
turn_end, turn_end_suspended, turn_prompt_row, turn_start, turn_suspended
usage, user_message, validate_replace_with_balance, warning


### uva-steering — 36 names

The policy layer: guards, gates, hints, hooks, and output
transformers.


`Action`, `AdmissionError`, `BudgetAwareTruncator`, `CATASTROPHIC_SHELL_PATTERNS`, `ContextBudgetHint`
`Gate`, `GateControl`, `GateState`, `Guard`, `GuardOutcome`
`GuardResult`, `GuardSeverity`, `Hint`, `HintMessage`, `HintPriority`
`HookDecision`, `HookEvent`, `InjectionPoint`, `IterationReminderHint`, `JsonExtractor`
`LoggingHook`, `LoopDetectionGuard`, `NoopGuard`, `NoopHint`, `NoopOutputTransformer`
`OutputTransformer`, `PromptInjectionGuard`, `RequestParams`, `ShellDenyGuard`, `ToolBudgetGuard`
`TransformerChain`, `TruncationHint`, `WorkspaceBoundaryGuard`, `event_name`, `run_guards`
`tool_fingerprint`


### uva-tool — 45 names

The tool paradigm: registry, traits, annotations, restriction, surface
projection.


`Context`, `DispatchPlan`, `DispatchRefusal`, `ExecutionMode`, `FULL_OUTPUT_TOOL_NAME`
`KEY_TOOLS`, `MCP_SELF_TOOL_PREFIX`, `OutputBound`, `OverflowStrategy`, `RegistrationScope`
`RegistrationToken`, `RenderKind`, `Result`, `RetrieveFullOutputTool`, `Risk`
`STRUCTURED_OUTPUT_TOOL_NAME`, `SanitizationPolicy`, `StreamOutput`, `SurfacePolicyError`, `Tool`
`ToolAnnotations`, `ToolCategory`, `ToolDescriptor`, `ToolEntry`, `ToolExposure`
`ToolFeatures`, `ToolMetadataSnapshot`, `ToolOutput`, `ToolOutputDefinition`, `ToolProvenance`
`ToolRegistry`, `ToolRegistryPlugin`, `ToolRestriction`, `ToolSchemaBuilder`, `ToolSurfacePolicy`
`TruncatedBy`, `Visibility`, `bound_output`, `called_tool_names`, `full_output_tool`
`is_mcp_self_tool`, `project_tool_definitions`, `register_tool_entries`, `signal_of`
`structured_output_tool`
