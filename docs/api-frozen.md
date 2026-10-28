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
| `elysper-core` | 41 |
| `elysper-context` | 41 |
| `elysper-harness` | 60 |
| `elysper-provider` | 89 |
| `elysper-session` | 115 |
| `elysper-steering` | 43 |
| `elysper-tool` | 46 |

The full lists are in [The frozen names](#the-frozen-names) below. Every
entry is spelled exactly as you would write it after the crate name:
`use elysper_harness::{ReActAgent, Agent}`.

A handful of these names exist only behind a Cargo feature or platform
flag (`OpenAICompatibleProvider` needs `openai`, the `AnthropicStream*`
types need `anthropic`, `signal_of` is unix-only). They are on the list
under the same rule: the *name* stays on the curated surface; the code
behind it still obeys its feature flag.

## What is *not* frozen

Be explicit about the other side of the promise:

- **Deep paths.** Anything reachable only by a longer path — for
  example `elysper_harness::agent::host` internals, or any `pub` item
  inside a module that lib.rs did not re-export — is **not** covered.
  Those names can move or disappear in any release without a major
  announcement. If you depend on one, tell us, and we will consider
  curating it.
- **Items defined in lib.rs outside the `pub use` lists.** A few lib.rs
  files define public items directly (e.g. the `Result` type alias in
  `elysper-core`). Those are not part of the curated lists and are not
  covered by this promise. (Where a name *is* re-exported — as
  `elysper-tool` re-exports `Result` from its `types` module — it is
  frozen like any other.)
- **Crate-root `pub mod` names.** `elysper_core::agent` (the module
  itself) is not on the curated list — only the names the crate root
  re-exports from it. A module path can move without a major bump.
- **The crates this page does not list.** The three crates added
  alongside the frozen seven — `elysper-plugin-host`,
  `elysper-plugin-protocol` and `elysper-harness-tokio` — are not
  frozen: their surfaces may still move before `0.2`. So is the
  facade (`elysper`), which only propagates the surfaces below.
- **The server's HTTP API.** `/api/v1` URLs version themselves; this
  page covers only the library crates listed above.
- **Signatures.** The promise is about the *set of names*: each listed
  name stays importable from the crate root, spelled exactly as listed.
  The shapes behind the names (parameters, generics, trait bounds)
  follow ordinary pre-1.0 evolution, recorded in the changelog.

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

## The gate

`make check-frozen-api` compares the live curated surface against
`scripts/frozen-api/baseline` (one `crate-name::Name` per line, sorted)
and fails on both kinds of drift:

```text
NEW:     elysper-harness::ShinyNewThing      # a name the baseline never promised
REMOVED: elysper-harness::ReActAgent         # a promised name that disappeared
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

### elysper-core — 41 names

Runtime seams without a runtime: clock, cancellation, ids, sleep, plus
small shared registries and text helpers.

`AgentDescriptor`, `AgentFilter`, `AtomicCancelToken`, `Blocking`, `BreakerConfig`
`BreakerObservation`, `BreakerPhase`, `BreakerState`, `CancelToken`, `Clock`
`Error`, `FixedClock`, `FixedSleeper`, `FullOutputStore`, `IdGen`, `InMemoryFullOutputStore`
`MAX_LIMIT`, `NameTable`, `Outcome`, `Registry`, `RegistryItem`, `RegistryList`
`SchemaViolation`, `Sensitive`, `SensitiveData`, `SequenceGenerator`, `SharedClock`
`SharedIdGen`, `Sleeper`, `SystemClock`, `ThreadBlocking`, `ThreadSleeper`
`UlidGenerator`, `cap_to_char_boundary`, `paginate_registry_list`, `panic_message`
`parse_judge_score`, `redact_partial`, `redact_partial_with`, `truncate_chars`
`validate_against_schema`

### elysper-context — 41 names

The context window: managers, memory, DAG summaries, compaction
settings.

`AgentState`, `CompactionDetails`, `CompactionLifecycle`, `CompactionRecord`
`CompactionSettings`, `ContextManager`, `DagConfig`, `DagContextManager`
`DagNode`, `DagStore`, `FileMemory`, `FileMemoryConfig`, `FileMemoryEntry`
`FileMemoryError`, `InMemoryMemory`, `MAX_MEMORY_INDEX_LINES`, `Memory`, `MemoryEntry`
`MemoryError`, `MemoryKind`, `MemoryScope`, `NoopContextManager`, `SUMMARY_TAG`
`SessionMemory`, `SharedContextManager`, `SummariseFn`, `SummarizingContextManager`
`SummaryId`, `ToolCallArchive`, `ToolCallRecord`, `ToolCallRecordKind`, `TouchedPair`
`TruncatingContextManager`, `TypedProjectionError`, `UsageMeter`, `ValidationError`
`events_to_messages`, `repair_session`, `should_compact`, `touched_target`
`typed_messages_from_sink`

### elysper-harness — 60 names

The agent loop: strategies, approval/resume, checkpoints, host hooks,
events, and the loop's own contract.

`Agent`, `AgentCaps`, `AgentDescriptor`, `AgentEntry`, `AgentEvent`, `AgentInput`
`AgentMeta`, `AgentRegistry`, `AgentRuntime`, `ApprovalMode`, `ApprovalPolicy`
`ApprovalRequest`, `Arbitration`, `CheckpointError`, `CheckpointSnapshot`
`Checkpointer`, `DEFAULT_MAX_ITERATIONS`, `DEFAULT_SYSTEM_PROMPT`, `DelegatingTool`
`DelegationCall`, `Diagnostic`, `DiagnosticKind`, `EventSink`, `Gate`, `GrantKey`
`GrantScope`, `Hook`, `Host`, `InMemoryCheckpointer`, `Plugin`, `PluginHandle`
`PromptContext`, `PromptSection`, `ReActAgent`, `ReActStrategy`, `ReasoningStrategy`
`Registration`, `RequestTransformer`, `ResultTransformer`, `ResumeCoverageError`
`ResumeDecision`, `ResumeStatus`, `ResumeToken`, `SessionEndReason`, `StrategyRegistry`
`SubagentWriters`, `Suspension`, `SystemEvent`, `WarningKind`, `align_decisions`
`check_coverage`, `default_descriptor`, `latest_request_snapshot_hash`, `may_retry`
`open_requests`, `prepare_messages_for_shot`, `single_shot_request`, `single_shot_request_shared`
`suspended_segment`, `workspace_hash`

### elysper-provider — 89 names

Model providers and everything around a completion: request/response
types, credentials, retry classification, cache policy, JSON repair.

`AnthropicProfile`, `AnthropicProvider`, `AnthropicStreamContentBlock`, `AnthropicStreamDelta`
`AnthropicStreamEvent`, `BilledClass`, `BlockAssembler`, `CacheControlMark`
`CachePolicy`, `CachePolicyApplier`, `CacheScope`, `CacheTtl`, `CompletionRequest`
`CompletionResponse`, `Content`, `ContentPart`, `ContextOverflowDetector`
`CredentialError`, `CredentialStatus`, `DEFAULT_ANTHROPIC_CONTEXT_WINDOW`
`DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS`, `DEFAULT_OPENAI_CONTEXT_WINDOW`, `DEFAULT_OPENAI_MAX_OUTPUT_TOKENS`
`DEFAULT_STREAM_IDLE_TIMEOUT`, `EMPTY_RESPONSE_MARKER`, `IDLE_TIMEOUT_MARKER`
`ImageContent`, `ImageDetail`, `MAX_PROVIDER_ERROR_BODY_CHARS`, `Message`
`MessageCacheStrategy`, `MessageKind`, `ModelCapabilities`, `ModelConfig`
`ModelInfo`, `ModelProvider`, `ModelTier`, `OpenAICompatibleProvider`, `OpenAIProfile`
`ProviderConfig`, `ProviderEntry`, `ProviderErrorBody`, `ProviderInfo`, `ProviderProfile`
`ProviderRegistry`, `ReasoningContent`, `ResourceLink`, `RetryClass`, `RetryConfig`
`Role`, `SamplingResult`, `StreamChunk`, `StubProfile`, `TextContent`, `TierLimits`
`TierParseError`, `TokenCounter`, `TokenUsage`, `ToolAnnotations`, `ToolArgsQuality`
`ToolChoice`, `ToolDefinition`, `ToolResult`, `ToolUse`, `WorkspaceConfig`
`apply_cache_policy`, `classify`, `classify_error`, `classify_provider_error_body`
`complete_partial_json`, `completion_to_sampling`, `empty_response_error`
`estimate_messages_token_count`, `idle_timeout_error`, `is_empty_response_error`
`is_idle_timeout`, `is_length_stop_reason`, `is_retryable_error`, `is_silent_overflow`
`normalize_api_key`, `parse_provider_error_body`, `parse_retry_after`, `parse_tool_input`
`parse_tool_input_logged`, `parse_tool_input_reported`, `parse_tool_input_with_completion`
`repair_json`, `retry_config_for`, `synthesize_orphan_result`

### elysper-session — 115 names

The durable ledger: JSONL sink and version header, events, history fold,
surface ledger, compaction, search, token metering, typed builders.

`AttachmentRefRow`, `BalanceCache`, `CURRENT_SCHEMA_VERSION`, `CompactionCheckpoint`
`CompactionError`, `CompactionLifecycleOutcome`, `CompactionLifecycleView`
`CompactionOutcome`, `CompactionPass`, `CompactionPassError`, `CompactionPlan`
`CompactionRecordView`, `CompactionRef`, `Compactor`, `ContextPressure`, `DEFAULT_BUS_CAPACITY`
`DEFAULT_LIMIT`, `DEFAULT_PRUNE_MIN_CHARS`, `DEFAULT_SUMMARY_TIMEOUT`, `EntryHit`
`FoldError`, `FoldedSurface`, `HistoryError`, `JsonlSessionSearch`, `KNOWN_SESSION_EVENT_TYPES`
`LogWriter`, `MAX_PLUGIN_KIND_BYTES`, `MAX_PLUGIN_PAYLOAD_BYTES`, `MappingGap`
`MeasurementBaseline`, `MetaMarker`, `OperationSnapshot`, `OperationState`
`OrphanedCompaction`, `PRUNED_TOOL_RESULT`, `ParkedPromptRecovery`, `PlannedCompaction`
`ReplaceRange`, `ResolvedReplace`, `SearchError`, `SearchQuery`, `SessionEndReason`
`SessionError`, `SessionEvent`, `SessionHit`, `SessionMetadataHeader`, `SessionSearch`
`SessionSink`, `SessionSnapshot`, `SharedSessionSearch`, `SharedSurfaceLedger`
`SnapshotBus`, `SnapshotReceiver`, `SummaryCompactor`, `SurfaceLedger`, `SurfaceOp`
`SurfaceToken`, `TOOL_NOT_STARTED`, `TOOL_OUTCOME_UNKNOWN`, `TURN_INTERRUPTED`
`TokenBudget`, `TokenMeasurement`, `TokenMeter`, `TokenMeterError`, `TypedEventRecord`
`UnbalancedCut`, `UsageBuckets`, `approval_asked`, `approval_decided`, `approval_granted`
`assemble_chunks`, `balanced_cut_at_or_before`, `balanced_message_cut`, `begin_compaction`
`build_balance_cache`, `compaction_balanced`, `compaction_outcomes`, `derive_history`
`empty_event_of`, `estimate_message_tokens`, `finish_compaction`, `fold_log_surface`
`fold_surface`, `interrupted_turn_closers`, `is_metadata_header_row`, `iteration_end`
`iteration_start`, `jsonl_session_search`, `log_tail_seq`, `modified_at`
`orphaned_compactions`, `parked_prompts`, `plan_compaction`, `prune_tool_results`
`request_header`, `request_snapshot`, `round_trip`, `step_end`, `step_end_suspended`
`step_start`, `structural_kind`, `subagent_enter`, `subagent_exit`, `surface_events_from_log`
`surface_message`, `surface_to_messages`, `tool_dispatch`, `try_fold_log_surface`
`turn_end`, `turn_end_suspended`, `turn_start`, `turn_suspended`, `usage`
`user_message`, `validate_replace_with_balance`

### elysper-steering — 43 names

The policy layer: guards, gates, hints, hooks, and output transformers.

`Action`, `AdmissionError`, `BudgetAwareTruncator`, `CATASTROPHIC_SHELL_PATTERNS`
`ContextBudgetHint`, `Gate`, `GateControl`, `GateState`, `Guard`, `GuardOutcome`
`GuardResult`, `GuardSeverity`, `Hint`, `HintMessage`, `HintPriority`, `HookDecision`
`HookEvent`, `HookHandler`, `HookHandlerWithState`, `HookMap`, `InjectionPoint`
`IterationReminderHint`, `JsonExtractor`, `LoggingHook`, `LoopDetectionGuard`
`NoopGuard`, `NoopHint`, `NoopOutputTransformer`, `OutputTransformer`, `PromptInjectionGuard`
`RequestParams`, `ShellDenyGuard`, `Steering`, `ToolBudgetGuard`, `TransformerChain`
`TruncationHint`, `WorkspaceBoundaryGuard`, `auto`, `event_name`, `for_tier`
`logging_hook_map`, `run_guards`, `tool_fingerprint`

### elysper-tool — 46 names

The tool paradigm: registry, traits, annotations, restriction, surface
projection.

`AdaptiveRegistry`, `Context`, `DispatchPlan`, `DispatchRefusal`, `ExecutionMode`
`FULL_OUTPUT_TOOL_NAME`, `GroupError`, `GroupedRegistry`, `MCP_SELF_TOOL_PREFIX`
`OutputBound`, `OverflowStrategy`, `RegistrationScope`, `RegistrationToken`
`RenderKind`, `RestrictedRegistry`, `Result`, `RetrieveFullOutputTool`, `Risk`
`STRUCTURED_OUTPUT_TOOL_NAME`, `SanitizationPolicy`, `StreamOutput`, `SurfacePolicyError`
`Tool`, `ToolAnnotations`, `ToolCategory`, `ToolDescriptor`, `ToolEntry`
`ToolExposure`, `ToolFeatures`, `ToolMetadataSnapshot`, `ToolOutput`, `ToolOutputDefinition`
`ToolProvenance`, `ToolRegistry`, `ToolRestriction`, `ToolSchemaBuilder`
`ToolSurfacePolicy`, `TruncatedBy`, `Visibility`, `bound_output`, `called_tool_names`
`full_output_tool`, `is_mcp_self_tool`, `project_tool_definitions`, `signal_of`
`structured_output_tool`
