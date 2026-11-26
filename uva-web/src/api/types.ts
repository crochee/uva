/**
 * Generated file — do not edit.
 *
 * Every interface below is compiled from the server's wire types:
 * `crates/uva-app-server/src/api/contract.rs` catalogues each named
 * response (and request) type with the RPC method that answers it, and
 * `examples/emit-contract.rs` prints that catalog as JSON Schema.
 *
 * Regenerate with `npm run generate:types` — `make check-web-contract`
 * fails when this file and the Rust types disagree.
 */

/**
 * One AG-UI event.
 */
export type AguiEvent =
  | {
      /**
       * The conversation the run belongs to.
       */
      threadId: string;
      /**
       * The run itself.
       */
      runId: string;
      type: 'RUN_STARTED';
    }
  | {
      /**
       * The conversation the run belongs to.
       */
      threadId: string;
      /**
       * The run that ended.
       */
      runId: string;
      type: 'RUN_FINISHED';
    }
  | {
      /**
       * What went wrong.
       */
      message: string;
      /**
       * The machine code, when the failure carries one.
       */
      code?: string | null;
      type: 'RUN_ERROR';
    }
  | {
      /**
       * The message the chunks belong to.
       */
      messageId: string;
      /**
       * Who speaks the message.
       */
      role: string;
      type: 'TEXT_MESSAGE_START';
    }
  | {
      /**
       * The message the chunk belongs to.
       */
      messageId: string;
      /**
       * The chunk itself — never the accumulated text.
       */
      delta: string;
      type: 'TEXT_MESSAGE_CONTENT';
    }
  | {
      /**
       * The message that ended.
       */
      messageId: string;
      type: 'TEXT_MESSAGE_END';
    }
  | {
      type: 'REASONING_START';
    }
  | {
      /**
       * The message the chunk belongs to.
       */
      messageId: string;
      /**
       * The chunk itself.
       */
      delta: string;
      type: 'REASONING_MESSAGE_CONTENT';
    }
  | {
      type: 'REASONING_END';
    }
  | {
      /**
       * The call's id — the same one its result carries.
       */
      toolCallId: string;
      /**
       * The tool the call addresses.
       */
      toolCallName: string;
      /**
       * The message the call belongs to.
       */
      parentMessageId?: string | null;
      type: 'TOOL_CALL_START';
    }
  | {
      /**
       * The call the chunk belongs to.
       */
      toolCallId: string;
      /**
       * The chunk itself.
       */
      delta: string;
      type: 'TOOL_CALL_ARGS';
    }
  | {
      /**
       * The call that ended.
       */
      toolCallId: string;
      type: 'TOOL_CALL_END';
    }
  | {
      /**
       * The message the result belongs to.
       */
      messageId: string;
      /**
       * The call the result answers.
       */
      toolCallId: string;
      /**
       * The result's content.
       */
      content: string;
      /**
       * Who produced the result.
       */
      role: string;
      type: 'TOOL_CALL_RESULT';
    };
/**
 * How risky a tool call is.
 *
 * The approval policy asks about the risky calls and lets the rest
 * through. `Unknown` is the default: a tool that did not classify
 * itself is neither safe nor dangerous, and the policy — not the
 * enum — decides what to do with it.
 */
export type Risk = 'low' | 'high' | 'unknown';
export type AudioFormat = 'wav' | 'mp3' | 'flac';
/**
 * Cache TTL class.
 *
 * `Ephemeral` is the default; providers may map this to short-lived cache
 * hints. `Extended` and `Long` request longer TTLs and require explicit
 * opt-in.
 */
export type CacheTtl = 'Ephemeral' | 'Extended' | 'Long';
/**
 * Namespace scope for a cache entry.
 *
 * The wrapped `String` is formatted as `u={user_id};s={session_id}` so that
 * different users can never collide on a cache key.
 */
export type CacheScope = string;
/**
 * How many components one slot accepts.
 */
export type Cardinality = 'one' | 'many';
export type Content =
  | {
      Single: ContentPart;
    }
  | {
      Multi: ContentPart[];
    };
export type ContentPart =
  | (TextContent & {
      type: 'text';
    })
  | (ImageContent & {
      type: 'image';
    })
  | (AudioContent & {
      type: 'audio';
    })
  | (ToolUse & {
      type: 'tool_use';
    })
  | (ToolResult & {
      type: 'tool_result';
    })
  | (ReasoningContent & {
      type: 'reasoning';
    })
  | (ResourceLink & {
      type: 'resource';
    });
export type ImageDetail = 'low' | 'high' | 'auto';
/**
 * One schedule expression. Adding a new variant is a
 * deliberate act — `Scheduler::tick` is the only place
 * that needs to learn the new firing semantics, and every
 * persistence layer round-trips through the same enum.
 */
export type JobKind =
  | {
      /**
       * Original cron expression string. Persisted for
       */
      expr: string;
      kind: 'cron';
    }
  | {
      /**
       * Re-fire cadence.
       */
      interval: Duration;
      kind: 'interval';
    }
  | {
      kind: 'once';
    };
/**
 * The session status — what a session reports to the outside.
 *
 * It is the controller state refined: a session with no log has
 * never run, and one whose log holds rows has run at least once.
 *
 * The **variant name is the wire spelling**, the same rule the row
 * vocabulary follows (ADR-0006): `rename_all = "snake_case"` derives
 * it, so the name and the string cannot drift apart.
 * [`SessionStatus::as_wire`] is the hand-written mirror of that
 * derivation — the pattern the vocabulary uses for `RowTag::as_wire`
 * — and a test pins the two together.
 */
export type SessionStatus = 'unspecified' | 'working' | 'input_required' | 'canceled' | 'completed';
/**
 * One durable row of a session log, as `sessions/get` returns it in
 * `history` and `follow` returns it in `rows`.
 *
 * The row's **payload is shaped by its tag**, so the schema says
 * which shape: a message row's `data` is the provider `Message` —
 * the same type the provider serializes — not an open bag. The rows
 * whose payload is free-form (a warning's message, a hook block's
 * policy, the crash-repair closer's recovery code) stay `unknown`:
 * the vocabulary declares their *tags*, not their payloads' shapes.
 *
 * The tag list comes from the vocabulary itself, so the union cannot
 * drift from the tag set.
 */
export type SessionTurn =
  | {
      type?: 'user_message';
      data?: Message;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'assistant_message';
      data?: Message;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'assistant_chunk';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'tool_result';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'compaction';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'compaction_start';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'compaction_summary';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'compaction_end';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'tool_call';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'tool_output';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'step';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'turn';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'iteration';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'warning';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'steering_guard';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'steering_hint';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'hook_block';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'subagent_enter';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'subagent_exit';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'request_header';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'approval_asked';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'approval_decided';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'approval_granted';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'tool_dispatch';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'request_snapshot';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'usage';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'sandbox_mode';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'session_shutdown';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'tool_result_transformed';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'parked_prompt';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'park_consumed';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'feedback';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    }
  | {
      type?: 'plugin';
      data?: unknown;
      /**
       * RFC 3339 instant the writer stamped the row with.
       */
      ts?: string | null;
      /**
       * The writer's ordinal for the row. This is the client's only
       * cursor, and what a resume token's `last_seq` points at.
       */
      seq?: number | null;
      /**
       * Provenance recorded beside the row, when it is not an ordinary
       * turn boundary. Today only `"steer"`.
       */
      source?: string | null;
      /**
       * `true` on a `tool_result` row whose tool failed.
       */
      is_error?: boolean | null;
      /**
       * The provider call id — a `tool_call` / `tool_output` row, an
       * approval row, a `tool_dispatch` row or a transformed result.
       */
      call_id?: string | null;
      /**
       * The provider call id a `tool_result` row answers.
       */
      tool_call_id?: string | null;
      attachments?: unknown;
      /**
       * The surface op the row applies instead of appending —
       * `"append"` (a plain string) or a replace range.
       */
      surface_op?: SurfaceOp | null;
      /**
       * `true` on a row whose kind this build does not know — the
       * writer's marker, letting such a build skip the row instead of
       * refusing the whole log.
       */
      ignorable?: boolean | null;
      /**
       * Which boundary the row is — a `step` / `turn` row. On a
       * `plugin` row it is the plugin's own event kind, a different
       * fact that happens to share the name.
       */
      kind?: string | null;
      /**
       * Which turn the row belongs to — a `step` / `turn` row.
       */
      turn?: number | null;
      /**
       * Which step inside the turn — a `step` row.
       */
      step?: number | null;
      /**
       * The chunk's ordinal within its turn — an `assistant_chunk` row.
       */
      chunk_seq?: number | null;
      /**
       * The provider stop reason — a terminal `assistant_chunk` row.
       */
      finish_reason?: string | null;
      /**
       * The correlation token of a compaction — the `compaction_start` /
       * `compaction_summary` / `compaction_end` rows.
       */
      token?: string | null;
      /**
       * How a compaction ended — `committed` / `failed` / `interrupted`.
       */
      outcome?: string | null;
      /**
       * The tool a `tool_call` row names.
       */
      name?: string | null;
      /**
       * The tool a row addresses — a `tool_output` row, an approval row
       * or a `tool_dispatch` row.
       */
      tool?: string | null;
      /**
       * The delegated child's session — a `subagent_enter` /
       * `subagent_exit` row.
       */
      child_session_id?: string | null;
      /**
       * The delegating session — a `subagent_enter` row.
       */
      parent_session_id?: string | null;
      /**
       * The agent a row names — a `subagent_enter` / `parked_prompt` row.
       */
      agent_name?: string | null;
      /**
       * How deep the delegation is — the root session is `0`.
       */
      depth?: number | null;
      /**
       * How a row ended — a `subagent_exit` / `approval_decided` row.
       */
      status?: string | null;
      /**
       * The provider a row names — a `request_header` /
       * `request_snapshot` / `parked_prompt` row.
       */
      provider?: string | null;
      /**
       * The model a row names — a `request_header` / `request_snapshot` /
       * `parked_prompt` row.
       */
      model?: string | null;
      /**
       * The request id an approval row refers to.
       */
      id?: string | null;
      /**
       * The digest of a held call's arguments — an `approval_asked` /
       * `tool_dispatch` row.
       */
      arguments_hash?: string | null;
      /**
       * The risk a held call was judged at — an `approval_asked` row.
       */
      risk?: string | null;
      /**
       * The agent a held call belongs to — an `approval_asked` row.
       */
      agent?: string | null;
      /**
       * Who decided — an `approval_decided` row.
       */
      by?: string | null;
      payload?: unknown;
      /**
       * What a grant covers — `"session"` / `"deployment"`.
       */
      scope?: string | null;
      /**
       * The workspace a grant is bound to.
       */
      workspace_hash?: string | null;
      /**
       * The digest of the sampling parameters — a `request_snapshot` row.
       */
      params_hash?: string | null;
      /**
       * The digest of the offered tool definitions — a `request_snapshot`
       * row.
       */
      tools_hash?: string | null;
      /**
       * The digest of the assembled system prompt — a `request_snapshot`
       * row.
       */
      system_hash?: string | null;
      /**
       * The prompt-section ids that contributed — a `request_snapshot`
       * row.
       */
      sections?: string[] | null;
      /**
       * The digest of the approval policy — a `request_snapshot` row.
       */
      policy_hash?: string | null;
      /**
       * The typed provider usage report — a `usage` row.
       */
      usage?: UsageBuckets | null;
      /**
       * The execution-policy override — a `sandbox_mode` row.
       */
      mode?: string | null;
      /**
       * The transformers that ran, in registration order.
       */
      transformers?: string[] | null;
      /**
       * The tool a transformed result belongs to.
       */
      tool_name?: string | null;
      /**
       * The digest of the result before a transformer rewrote it.
       */
      before?: string | null;
      /**
       * The digest of the result the model reads.
       */
      after?: string | null;
      /**
       * The handle a parked prompt is spent by — a `parked_prompt` /
       * `park_consumed` row.
       */
      park_id?: string | null;
      /**
       * The parked prompt's text.
       */
      text?: string | null;
      /**
       * `true` when the turn was parked by a `Rerun`.
       */
      rerun?: boolean | null;
      /**
       * The ordinal of the message a `feedback` row rates.
       */
      message_id?: string | null;
      /**
       * `true` for a thumbs-up, `false` for a thumbs-down.
       */
      thumbs_up?: boolean | null;
      /**
       * The payload's encoded size, when a cap truncated it — a
       * `plugin` row.
       */
      original_bytes?: number | null;
    };
export type Role = 'system' | 'user' | 'assistant' | 'tool';
/**
 * How a [`SessionEvent`] enters the model-visible surface.
 *
 * Wire format:
 *
 * - `"append"` (string) — plain append at the tail.
 * - `{"start": usize, "end": usize, "source_event_seqs":
 *   [u64]}` (struct) — atomic replacement of `[start, end)`
 *   in the folded surface. `source_event_seqs` cites the seqs
 *   being shadowed so the fold can reject provenance violations.
 */
export type SurfaceOp =
  | string
  | {
      /**
       * Inclusive start index of the replaced range (in the
       * folded surface).
       */
      start: number;
      /**
       * Exclusive end index of the replaced range.
       */
      end: number;
      /**
       * Seqs of the events being shadowed. The fold validator
       * rejects replace ops whose `source_event_seqs` either
       * (a) reference seqs outside the previous-log prefix or
       * (b) are non-monotonic with the new event's seq.
       */
      source_event_seqs: number[];
    };
/**
 * Stable opaque identifier for one [`Job`]. ULID-encoded so
 * the lexicographic order of ids also encodes creation
 * order — useful for the store's "next job to fire"
 * queries and for `unfire`-style debugging (newer job ids
 * sort higher).
 */
export type JobId = string;
/**
 * Lifecycle status. `Active` is the only state that emits
 * firings. `Paused` keeps the schedule on disk but stops
 * ticking. `Completed` is terminal — the store's
 * `gc_completed()` sweep is the only thing that removes
 * such rows.
 */
export type JobStatus = 'active' | 'paused' | 'completed';
/**
 * The four outcomes of one profile transaction (D37).
 */
export type ProfileChangeState = 'applied' | 'restart-required' | 'overridden' | 'failed';
/**
 * Which assembly segment a profile row sits in.
 */
export type ProfileSegment = 'core' | 'ui';
/**
 * Render-kind hint for UI consumers. Serialises in
 * `snake_case` (matching the rest of the API surface:
 * `ContentPart`, `OperationState`), so `"shell"` / `"web_fetch"`
 * / `"todo"` ride the wire.
 */
export type RenderKind = 'text' | 'shell' | 'read' | 'write' | 'web_fetch' | 'todo' | 'json';
/**
 * The operator's answer to one held call.
 *
 * [`ResumeStatus::Resolved`] lets the call execute;
 * [`ResumeStatus::Cancelled`] and [`ResumeStatus::Rejected`] record
 * the decision and write a model-visible `is_error` tool result
 * instead of executing it. [`ResumeStatus::TimedOut`] is the
 * deadline's own answer: nobody replied, so the call did not run —
 * but the model is told *that*, not that an operator said no.
 */
export type ResumeStatus = ('resolved' | 'cancelled' | 'rejected') | 'timed_out';
/**
 * How a submitted message should reach its run.
 *
 * `Queue` is the ordinary path: the turn waits for the session and
 * the run that starts it logs the user row. `Steer` asks for the
 * message to be injected into the run that is live *now*, through
 * the session log — and reports `queue` back when no run was live
 * to receive it.
 */
export type SteerDelivery = 'queue' | 'steer';
/**
 * The closed vocabulary of typed row tags — the `type` strings a
 * durable session row written through the typed layer carries.
 *
 * The variant name *is* the wire tag: the enum derives its entire
 * spelling from `rename_all = "snake_case"`, so the name and the
 * wire string cannot drift apart. The enum also derives
 * [`JsonSchema`], publishing the tags as a `oneOf` of consts — the
 * union the wire contract (and the frontend types generated from
 * it) derives from, replacing a hand-written mirror.
 *
 * The vocabulary only grows, and only here: a new tag makes every
 * exhaustive match over this enum (this module's, and every
 * consumer's) fail to compile until the new row says what it is.
 */
export type SessionEventType =
  | 'user_message'
  | 'assistant_message'
  | 'assistant_chunk'
  | 'tool_result'
  | 'compaction'
  | 'compaction_start'
  | 'compaction_summary'
  | 'compaction_end'
  | 'tool_call'
  | 'tool_output'
  | 'step'
  | 'turn'
  | 'iteration'
  | 'warning'
  | 'steering_guard'
  | 'steering_hint'
  | 'hook_block'
  | 'subagent_enter'
  | 'subagent_exit'
  | 'request_header'
  | 'approval_asked'
  | 'approval_decided'
  | 'approval_granted'
  | 'tool_dispatch'
  | 'request_snapshot'
  | 'usage'
  | 'sandbox_mode'
  | 'session_shutdown'
  | 'tool_result_transformed'
  | 'parked_prompt'
  | 'park_consumed'
  | 'feedback'
  | 'plugin';

export interface AgentDetail {
  name: string;
  description: string;
  kind: string;
  version: string;
  instructions: string;
  capabilities: string[];
  tools: string[];
  handoffs: string[];
  /**
   * Effective per-run iteration cap after `[1, 4096]`
   * clamping — the loop actually applies this value, not
   * whatever the caller asked for. `None` means the
   * descriptor did not record a hint and the runtime
   * default applies.
   */
  max_iterations?: number | null;
  model_hint?: string | null;
  owner?: string | null;
  domain?: string | null;
  persona?: string | null;
  /**
   * `true` for the names listed in `PROTECTED_AGENTS`;
   * `false` for agents registered at runtime.
   */
  protected: boolean;
}
/**
 * One held tool call, waiting for an approval decision.
 *
 * The request is the unit an operator approves: it names the call,
 * the tool, a digest of the arguments the call was made with, and
 * the risk the tool declared. The digest is what lets a decision be
 * checked against the call it was made for — an approval for one
 * set of arguments is not an approval for another.
 */
export interface ApprovalRequest {
  /**
   * The request's identity — what an approval decision refers to.
   */
  id: string;
  /**
   * The provider's call id for the held call.
   */
  call_id: string;
  /**
   * The tool the call addresses.
   */
  tool: string;
  /**
   * A digest of the call's arguments.
   */
  arguments_hash: string;
  /**
   * The risk the tool declared for this call.
   */
  risk: Risk;
  /**
   * The agent the call belongs to.
   */
  agent: string;
}
export interface AudioContent {
  data: string;
  mime_type: string;
  format: AudioFormat | null;
}
/**
 * Marks a prompt snapshot for cache control tracking.
 *
 * All fields participate in the mark's hash, so a change to any of
 * them is distinguishable from a change to the system prompt content.
 */
export interface CacheControlMark {
  ttl: CacheTtl;
  scope: CacheScope;
  pinned: boolean;
}
/**
 * What a manual compaction pass did.
 */
export interface CompactResult {
  /**
   * `true` when a summary replaced its span.
   */
  committed: boolean;
  /**
   * First shadowed surface position (fold index).
   */
  start: number | null;
  /**
   * One past the last shadowed surface position.
   */
  end: number | null;
  /**
   * Messages the replace shadowed.
   */
  shadowed_messages: number;
  /**
   * Pre-prune token price of the shadowed span.
   */
  shadowed_tokens: number;
  /**
   * The replacement text.
   */
  summary?: string | null;
  /**
   * First shadowed log ordinal.
   */
  first_seq?: number | null;
  /**
   * Last shadowed log ordinal.
   */
  last_seq?: number | null;
  /**
   * Why nothing was committed (`under_budget`, `empty_span`).
   */
  reason?: string | null;
}
export interface TextContent {
  text: string;
  cache_control?: CacheControlMark | null;
}
export interface ImageContent {
  data: string;
  mime_type: string;
  detail: ImageDetail | null;
}
export interface ToolUse {
  id: string;
  name: string;
  input: unknown;
}
export interface ToolResult {
  tool_use_id: string;
  /**
   * Optional tool name. Carried on the wire so downstream
   * consumers (SSE mapping, frontend segment rendering) can
   * label a `tool_result` even when no preceding `tool_call`
   * segment exists in the same message (e.g. results flushed
   * by an interceptor or replayed from session JSONL).
   * `#[serde(default)]` keeps deserialization backward-compatible
   * with payloads written before this field existed.
   */
  tool_name?: string | null;
  content: ContentPart[];
  structured_content: unknown;
  is_error: boolean | null;
  /**
   * Structured metadata accompanying the tool result (counts,
   * timing, truncation reason). Defaults to empty for
   * backward compatibility with payloads written before this
   * field existed.
   *
   * The field is forwarded from `uva_tool::ToolOutput`'s
   * `metadata` so the LLM and the frontend
   * can see tool-attached telemetry without parsing the
   * content stream.
   */
  metadata?: {
    [k: string]: unknown;
  };
  truncated_by?: unknown;
}
export interface ReasoningContent {
  text: string;
  /**
   * Anthropic `signature_delta` value attached to the most recent
   * reasoning block. Required to preserve cross-turn reasoning
   * continuity when the upstream Provider is Anthropic.
   */
  signature?: string | null;
}
export interface ResourceLink {
  uri: string;
  name: string;
  title: string | null;
  description: string | null;
  mimeType: string | null;
}
/**
 * `POST /api/v1/schedules` body. Mirrors `CreateScheduleRequest`
 * in `uva-web/src/api/types.ts`.
 */
export interface CreateScheduleRequest {
  /**
   * Operator-facing name. The store's `has_name` enforces
   * uniqueness; the route returns 409 on a duplicate.
   */
  name: string;
  description?: string;
  kind: JobKind;
  payload?: unknown;
  /**
   * Optional override for the first fire; if absent the
   * server computes it from the kind.
   */
  next_fire_at?: string | null;
}
export interface Duration {
  secs: number;
  nanos: number;
}
/**
 * Response from `POST /api/v1/skills`.
 */
export interface CreateSkillResponse {
  name: string;
  path: string;
}
/**
 * An evaluation report: every case result plus aggregates.
 */
export interface EvalReport {
  /**
   * Suite name.
   */
  suite_name: string;
  /**
   * When the run finished (UTC, RFC 3339 in JSON).
   */
  generated_at: string;
  /**
   * Individual test results, in suite order.
   */
  results: TestResult[];
  /**
   * Average score across all case × metric scores.
   */
  average_score: number;
  /**
   * Number of cases that passed.
   */
  passed: number;
  /**
   * Total number of cases.
   */
  total: number;
  /**
   * R-OPT-HPC: number of trials per case (set by
   * `EvalRunner::run_trials(n)`; `1` for `EvalRunner::run`).
   * Used by `pass_at(k)` / `pass_pow(k)` / `per_case_pass_rate`.
   */
  trials: number;
  /**
   * R-OPT-HPC: per-case pass counts (filled by `run_trials`;
   * empty for `run`). Together with `trials` it gives
   * `per_case_pass_rate`.
   *
   * Layout: `case_id -> count_of_passes_across_trials`.
   * `tests` are excluded from the counts (transcript carries
   * the `Error` verdict for them; `pass_at` only counts
   * `Pass { score }`).
   */
  per_case_passes: {
    [k: string]: number;
  };
  /**
   * R-OPT-HPC: total trials attempted per case. Together
   * with `per_case_passes` it gives `per_case_pass_rate`.
   * Counts `Pass` + `Fail` + `Cancelled`; excludes
   * `Error` (a backend fault, not a real trial outcome).
   */
  per_case_attempts: {
    [k: string]: number;
  };
}
/**
 * A single test case result.
 */
export interface TestResult {
  /**
   * Test case ID.
   */
  case_id: string;
  /**
   * The actual output produced by the agent.
   */
  actual_output: string;
  /**
   * Metric scores (`BTreeMap` so export ordering is
   * deterministic), each in `0.0..=1.0`.
   */
  scores: {
    [k: string]: number;
  };
  /**
   * Whether the case passed (all scores ≥ threshold).
   */
  passed: boolean;
}
/**
 * A suite of evaluation test cases.
 */
export interface EvalSuite {
  /**
   * Suite name. `pub(crate)` so the `Serialize` derive can
   * read it directly; consumers use the [`Self::name`] getter.
   */
  name: string;
  /**
   * Cases in insertion order. `pub(crate)` for the same reason.
   */
  cases: TestCase[];
}
/**
 * A single evaluation test case.
 */
export interface TestCase {
  /**
   * Test case identifier (also the CSV `case_id`).
   */
  id: string;
  /**
   * Input prompt handed to the agent under test.
   */
  input: string;
  /**
   * Expected keywords (for [`KeywordMetric`] and the
   * runner's no-metrics fallback).
   */
  expected_keywords: string[];
  /**
   * Optional expected exact output, recorded with the case.
   */
  expected_output: string | null;
}
/**
 * The session-level facts a client needs before its first row.
 */
export interface FollowHeader {
  /**
   * The session id.
   */
  session_id: string;
  /**
   * The session's owner.
   */
  user_id: string;
  /**
   * The agent the session runs as (delegated children name the
   * peer that ran them).
   */
  agent_name?: string | null;
  /**
   * The session status — the same closed set the session listing
   * and the session detail carry. The `follow` header used to
   * speak its own four-label vocabulary (`idle` / `running` /
   * `awaiting_approval` / `cancelled`); retiring it means a client
   * asking "what state is this session in" gets one answer, not
   * two.
   */
  state: SessionStatus;
  /**
   * First durable row's timestamp.
   */
  created_at?: string | null;
  /**
   * Last durable row's timestamp.
   */
  updated_at?: string | null;
}
/**
 * The complete first frame of a `follow` stream.
 *
 * The type is the wire contract's owner: a client decodes the
 * baseline into this very type, so the shape the server publishes
 * and the shape a client reads cannot drift apart.
 */
export interface FollowSnapshot {
  /**
   * Session-level facts.
   */
  header: FollowHeader;
  /**
   * The last [`FOLLOW_WINDOW`] durable rows.
   */
  rows: SessionTurn[];
  /**
   * The ordinal of the last durable row (`0` for an empty log).
   */
  cursor: number;
  /**
   * The in-progress turn's transient projection.
   */
  live: LiveTurn;
  /**
   * Context occupancy for the next request.
   */
  context: SessionContextMeter;
  /**
   * Interrupts still waiting for a decision.
   */
  pending_interrupts: ApprovalRequest[];
  /**
   * Fingerprint of the log's newest request envelope (`""` when
   * the log has none).
   */
  request_snapshot_hash: string;
}
export interface Message {
  role: Role;
  content: Content;
  tool_call_id: string | null;
  name: string | null;
  tool_result_cleared_at: string | null;
}
/**
 * Disjoint provider usage buckets for one report or one folded
 * log.
 *
 * The four buckets never overlap: `cache_read` / `cache_write`
 * are separate from `input`, and reasoning tokens are already
 * included in `output`. Every field carries a serde default so
 * partial payloads (and pre-R30 rows) keep parsing.
 */
export interface UsageBuckets {
  /**
   * Uncached prompt tokens the provider billed.
   */
  input_tokens: number;
  /**
   * Completion tokens (reasoning included, not double-counted).
   */
  output_tokens: number;
  /**
   * KV-cache read tokens; `None` when the provider did not
   * report cache metrics.
   */
  cache_read_tokens?: number | null;
  /**
   * KV-cache write tokens; `None` when unreported.
   */
  cache_write_tokens?: number | null;
}
/**
 * The transient projection of the step that is running (or just
 * ended) in this process.
 */
export interface LiveTurn {
  /**
   * The log position where this projection's step began.
   *
   * A durable content row whose ordinal is greater than this
   * supersedes the projection: the row is that step's own answer.
   * `0` means "no position" — a projection whose step began before
   * this process knew the log's numbering (a remote owner's step,
   * projected here).
   */
  start_seq: number;
  /**
   * Monotone per-controller frame counter. A baseline carries the
   * current value; a client discards an increment whose `live_seq`
   * is not greater than its baseline.
   */
  live_seq: number;
  /**
   * Whether the producing run is still live.
   */
  running: boolean;
  /**
   * Text streamed (and committed) so far.
   */
  text: string;
  /**
   * Reasoning streamed so far.
   */
  reasoning: string;
  /**
   * Tool calls proposed by the turn, in call order.
   */
  tool_calls: LiveToolCall[];
}
/**
 * One tool call the in-progress turn has produced.
 */
export interface LiveToolCall {
  /**
   * The provider's call id.
   */
  call_id: string;
  /**
   * The tool the call addresses.
   */
  name: string;
  arguments: unknown;
  /**
   * `running`, `completed` or `failed`.
   *
   * A `String` and not a `&'static str`: the projection is
   * published on the wire, and a wire type a client cannot decode
   * back is a shape with two owners.
   */
  status: string;
  /**
   * The call's output: while it runs, the streamed chunks appended
   * in arrival order; once it settles, the result text.
   */
  output?: string | null;
}
/**
 * How much of the model's window the next request would occupy.
 */
export interface SessionContextMeter {
  /**
   * Tokens the next request is projected to carry.
   */
  used: number;
  /**
   * The model's advertised window.
   */
  window?: number | null;
}
/**
 * Response of `POST /api/v1/sessions/{id}/fork`.
 */
export interface ForkSessionResponse {
  /**
   * The new session — addressable, listable and streamable exactly
   * like any other conversation.
   */
  session_id: string;
  /**
   * The session it was branched from.
   */
  forked_from: string;
  /**
   * Durable rows the child inherited.
   */
  rows: number;
}
/**
 * Re-export of `FiredJob` under a JSON-friendly shape.
 */
export interface JobInfo {
  id: string;
  name: string;
  fired_at: string;
  payload: unknown;
}
/**
 * One provider's advertised model.
 */
export interface ModelEntry {
  /**
   * The provider's `[providers.<name>]` entry name.
   */
  provider: string;
  /**
   * The provider's default model id.
   */
  model: string;
  /**
   * The model's context window, in tokens.
   */
  context_window: number;
  /**
   * Whether the provider accepts tool calls.
   */
  supports_tools: boolean;
  /**
   * Whether the provider streams.
   */
  supports_streaming: boolean;
}
/**
 * Bare models-listing response.
 */
export interface ModelsResponse {
  /**
   * One entry per configured provider.
   */
  models: ModelEntry[];
  /**
   * The provider a turn runs under when it names none.
   */
  default_provider: string;
  /**
   * The model a turn runs under when it names none.
   */
  default_model: string;
}
/**
 * The transaction's report — the answer of `profile/set`,
 * `profile/create`, and `profile/delete`.
 *
 * A caller must render the receipt, never assume success: the four
 * states say what actually happened.
 */
export interface ProfileChange {
  /**
   * The outcome state.
   */
  state: ProfileChangeState;
  /**
   * Why: the diff's affected ids, the override layer, or the error.
   */
  reasons: string[];
  /**
   * The mount diff, when the change reached the tree.
   */
  diff?: ProfileDiff | null;
}
/**
 * What changed between two resolved profiles, and therefore which
 * fibers a running process must remount.
 *
 * `added`, `removed`, and `remounted` name **core** unit ids only: a
 * UI bundle change is the frontend loader's concern and never remounts
 * a core fiber.
 */
export interface ProfileDiff {
  /**
   * Ids present in the new assembly but not the old one.
   */
  added: string[];
  /**
   * Ids present in the old assembly but not the new one.
   */
  removed: string[];
  /**
   * Ids whose declaration or non-volatile configuration changed.
   */
  remounted: string[];
  /**
   * Ids whose only configuration change was in a volatile field.
   */
  volatile_changed: string[];
  /**
   * The profile disabled HMR; the new configuration needs a restart.
   */
  restart_required: boolean;
}
/**
 * The `profile/changed` broadcast's payload.
 *
 * A committed write (one that reached the tree) is announced on this
 * unscoped notification so every client re-reads the row it changed.
 * `state` is the transaction's outcome, `ids` the rows it touched, and
 * `at` the instant the broadcast was made.
 *
 * The type exists so the notification has the same named shape the
 * five methods answer with: the emitter derives a schema from it and
 * the frontend subscribes with the generated type.
 */
export interface ProfileChangedEvent {
  /**
   * Who asked for the write (`"ui"` on this transport, an agent
   * name through the self-MCP tool).
   */
  actor: string;
  /**
   * The transaction's outcome.
   */
  state: ProfileChangeState;
  /**
   * The ids the write touched.
   */
  ids: string[];
  /**
   * When the write committed.
   */
  at: string | null;
}
/**
 * One row of the resolved profile, as `profile/list` returns them and
 * `profile/get` returns one of.
 *
 * The row is the settings page's data source: the interface is the
 * resolved profile read backwards (D26). `inject` is the row's
 * **effective** dependency set — the plugin manifest's default unioned
 * with the row's declaration.
 */
export interface ProfileRow {
  /**
   * The row's stable id.
   */
  id: string;
  /**
   * Which assembly segment the row sits in.
   */
  segment: ProfileSegment;
  /**
   * The plugin the row loads (empty for a group row).
   */
  name: string;
  /**
   * Whether the row is a nested group.
   */
  group: boolean;
  /**
   * Whether the row (and its descendants, for a group) stays out of
   * the assembly.
   */
  disabled: boolean;
  config: unknown;
  /**
   * Host capabilities the row grants.
   */
  capabilities: string[];
  /**
   * The effective dependency set.
   */
  inject: string[];
  intercept?: unknown;
  isolate?: unknown;
  /**
   * `mounted`, or `off: <reason>` when the assembly skipped it.
   */
  status: string;
  /**
   * The kernel's plugin state, when the row is mounted.
   */
  plugin_state: string | null;
}
/**
 * Response from `POST /api/v1/skills/reload`.
 */
export interface ReloadSkillsResponse {
  /**
   * Number of skills found in the workspace after reload.
   */
  count: number;
}
/**
 * One decision from the client. `Resolved` approves this call;
 * `Cancelled`/`Rejected` writes the model-visible is_error result
 * instead of executing it. The resume must cover every open
 * interrupt.
 */
export interface ResumeDecision {
  /**
   * `ApprovalRequest::id` (or its `call_id`) this answers.
   */
  id: string;
  /**
   * What the operator decided.
   */
  status: ResumeStatus;
  payload?: unknown;
}
/**
 * The token that resumes a suspended run.
 *
 * It is derived from the log, not from memory: `last_seq` is the
 * tail the caller saw, `pending` is the set of call ids still
 * waiting for a decision, and `request_snapshot_hash` binds the
 * resume to the request envelope that was suspended.
 *
 * A log-only reader derives the same token without an in-memory
 * handoff: `last_seq` is the maximum row ordinal, `pending` is the
 * `approval_asked` call ids of the last suspended turn minus those
 * with an `approval_decided` row, and `request_snapshot_hash` is the
 * fingerprint of the last `request_snapshot` row (provider, model,
 * parameter/tool/system/policy hashes and section ids; seq and ts
 * excluded).
 */
export interface ResumeToken {
  /**
   * The tail ordinal the suspension ended at.
   */
  last_seq: number;
  /**
   * Call ids still awaiting a decision, sorted for comparison.
   */
  pending: string[];
  /**
   * Fingerprint of the request envelope at suspension time.
   */
  request_snapshot_hash: string;
}
/**
 * One row of the deployment's plugin-tree audit: what the row is,
 * and — when it is not usable — why.
 *
 * The wire shape of `profile/audit`: `id` and `name` identify the
 * row, `state` says which list it belongs to, `missing` names the
 * service keys a pending row still waits for.
 */
export interface RowAudit {
  /**
   * The row's id (its own, not its factory's).
   */
  id: string;
  /**
   * The row's factory name.
   */
  name: string;
  /**
   * `failed` (never started, or the activation rejected) or
   * `pending` (waiting for a service the composition does not
   * supply). A row in neither list is usable.
   */
  state: string;
  /**
   * The service keys a pending row still waits for.
   *
   * Empty for a failed row — the failure is the activation's, not
   * a dependency's.
   */
  missing: string[];
}
/**
 * `GET /api/v1/schedules/{id}` — full row.
 */
export interface Schedule {
  /**
   * Stable id (ULID by default).
   */
  id: JobId;
  /**
   * Operator-facing name. The store's `has_name` check
   * enforces uniqueness per-store so two jobs cannot
   * shadow each other in operator dashboards.
   */
  name: string;
  /**
   * Free-form description for operator dashboards.
   */
  description: string;
  /**
   * Schedule kind. Drives `next_fire`.
   */
  kind: JobKind;
  payload: unknown;
  /**
   * Wall-clock time the scheduler should next emit this
   * job. Persisted so reloads survive a process restart.
   */
  next_fire_at: string;
  /**
   * Last firing time (UTC). `None` until the first tick
   * actually fires.
   */
  last_fired_at: string | null;
  /**
   * Current lifecycle state.
   */
  status: JobStatus;
}
/**
 * `GET /api/v1/schedules` — list row.
 */
export interface ScheduleSummary {
  id: string;
  name: string;
  kind: JobKind;
  status: JobStatus;
  next_fire_at: string;
}
/**
 * Response from `POST /api/v1/schedules/{id}/tick`.
 */
export interface ScheduleTick {
  fired: JobInfo[];
}
/**
 * One cross-domain hit in the response.
 */
export interface SearchHit {
  domain: string;
  id: string;
  title: string;
  score: number;
  preview?: string | null;
}
export interface SendChatMessageResponse {
  message_id: string;
  /**
   * `true` when the turn waits for the session's next run.
   *
   * A queued turn is **on disk** when this answer goes out: the
   * controller parks it as a durable `parked_prompt` row and
   * flushes it before the route answers. The answer therefore
   * promises what the log holds — a crash between the answer and
   * the run cannot lose the message.
   */
  queued: boolean;
  /**
   * The delivery that actually happened: `"queue"` when no run was
   * live to steer, `"steer"` when the message was injected.
   */
  delivery: SteerDelivery;
}
/**
 * One legacy attachment slot of a session detail.
 *
 * Modern sessions always send `[]`: tool calls and their results
 * travel through [`SessionTurn`] rows now. The type survives because
 * the detail page still renders sessions that predate that.
 */
export interface SessionArtifact {
  /**
   * The attachment's stable id.
   */
  attachmentId?: string | null;
  /**
   * Display name.
   */
  name?: string | null;
  /**
   * One-line description.
   */
  description?: string | null;
  /**
   * The attachment's content segments.
   */
  parts?: SessionPart[] | null;
  /**
   * Server-side metadata the legacy reader groups on.
   */
  metadata?: SessionArtifactMetadata | null;
}
/**
 * One content segment of a legacy attachment.
 *
 * The v1 server's content parts are field-presence discriminated, so
 * at most one of `text` / `raw` / `url` / `data` is present per part.
 */
export interface SessionPart {
  /**
   * Text the segment carried.
   */
  text?: string | null;
  data?: unknown;
  /**
   * Base64 payload the segment carried.
   */
  raw?: string | null;
  /**
   * Remote URL the segment pointed at.
   */
  url?: string | null;
  /**
   * File name the segment carried.
   */
  filename?: string | null;
  /**
   * MIME type the segment carried.
   */
  mediaType?: string | null;
}
/**
 * The metadata block a legacy attachment carries.
 */
export interface SessionArtifactMetadata {
  /**
   * The legacy discriminator (`tool_call` / `tool_result`).
   */
  kind?: string | null;
  /**
   * The tool the recorded call addressed.
   */
  tool_name?: string | null;
  /**
   * The provider call id the call and its result share.
   */
  tool_use_id?: string | null;
  /**
   * Whether a recorded tool result failed.
   */
  is_error?: boolean | null;
}
/**
 * Detailed session view including history. The `history`
 * field is a JSON-encoded array of session events (one
 * entry per agent / system frame persisted to the session
 * sink). The frontend's `SessionDetailPage` reads the entries
 * individually and runs them through the same renderer the
 * live `ChatPage` uses.
 */
export interface SessionDetail {
  id: string;
  /**
   * The session status — the same closed set the summary carries.
   */
  status: SessionStatus;
  context_id: string;
  /**
   * When the session **started**: the log's first event's
   * timestamp — the same value [`SessionSummary::created_at`]
   * reports, so the listing and this view never disagree.
   */
  created_at?: string | null;
  /**
   * When the session was last **active**: the log's last event's
   * timestamp.
   */
  updated_at?: string | null;
  history: SessionTurn[];
  /**
   * Legacy field — kept as an empty array so the
   * SessionDetailPage doesn't have to special-case undefined.
   * The modern equivalents (`ContentPart::Raw`,
   * `ContentPart::Resource`) are inlined into `history`.
   */
  artifacts: SessionArtifact[];
  /**
   * Sub-agent provenance — see [`SessionSummary::parent_session_id`].
   */
  parent_session_id?: string | null;
  agent_name?: string | null;
}
/**
 * Frontend-facing session summary.
 */
export interface SessionSummary {
  id: string;
  /**
   * The session status — one of the five the status module
   * produces. The wire spells it `unspecified`, `working`,
   * `input_required`, `canceled` or `completed`.
   */
  status: SessionStatus;
  context_id?: string | null;
  /**
   * When the session **started**: the log's first event's
   * timestamp, never `now()` and never the last activity. The
   * default sort (`-created_at`) is therefore "newest session
   * first"; the frontend labels the field "started".
   */
  created_at?: string | null;
  /**
   * Present when this session was opened by a delegated
   * sub-agent (the `task` tool): the session the delegation
   * ran in. A first-class session either way — the field only
   * carries provenance, never privileges.
   */
  parent_session_id?: string | null;
  /**
   * The registry name of the peer agent that ran this session
   * (sub-agent sessions only).
   */
  agent_name?: string | null;
}
/**
 * Session-scoped token usage. Deliberately **not** the process-wide
 * counters at `/api/v1/chat/usage`: a client showing one conversation
 * must not be billed for every other session this process ran.
 */
export interface SessionUsage {
  /**
   * The session the numbers belong to.
   */
  session_id: string;
  /**
   * Uncached prompt tokens across the session's log.
   */
  input_tokens: number;
  /**
   * Completion tokens (reasoning included, not double-counted).
   */
  output_tokens: number;
  /**
   * KV-cache read tokens, when providers reported them.
   */
  cache_read_tokens?: number | null;
  /**
   * KV-cache write tokens, when providers reported them.
   */
  cache_write_tokens?: number | null;
  /**
   * Turns that reached `turn/end`.
   */
  turns: number;
  /**
   * Tokens the next request is projected to carry.
   */
  context_used: number;
  /**
   * The model's advertised window.
   */
  context_window?: number | null;
}
export interface Skill {
  name: string;
  description: string;
}
/**
 * Full detail returned by `GET /api/skills/:id`.
 *
 * In addition to the short `description` (used in list rows), the
 * detail payload exposes the raw frontmatter map and the markdown
 * body so the UI can render the SKILL.md as structured metadata
 * plus rendered markdown — no second round-trip needed.
 */
export interface SkillDetail {
  name: string;
  description: string;
  path: string;
  /**
   * Parsed YAML/JSON frontmatter of SKILL.md, with `name` /
   * `description` / `metadata` keys flattened into a flat
   * `key → value` map for easy table rendering on the client.
   */
  frontmatter: {
    [k: string]: unknown;
  };
  /**
   * Markdown body (everything after the frontmatter closer).
   * Empty string when the file has no body.
   */
  body: string;
}
/**
 * `TaskDelegation` — one delegation, flattened for the frontend.
 * Wire shape mirrors `uva-web/src/api/types.ts::TaskDelegation`.
 */
export interface TaskDelegation {
  /**
   * Synthetic id `<parentSessionId>:<childSessionId>` — survives a
   * refresh, and names one delegation for good (see
   * `delegation_id`).
   */
  id: string;
  parent_session_id: string;
  child_session_id: string | null;
  parent_depth: number;
  agent: string;
  prompt: string;
  started_at: string | null;
  finished_at: string | null;
  status: string;
}
export interface Tool {
  name: string;
  description: string;
}
export interface ToolDetail {
  name: string;
  description: string;
  input_schema: unknown;
  /**
   * `core` for tools compiled into the binary, `dynamic`
   * for tools registered at runtime.
   */
  provenance: string;
  /**
   * R17: the tool's canonical rendering contract
   * (`RenderKind` + title + presentation hints). Consumers
   * (UI, replay) pick a renderer from this instead of
   * hard-coding per-tool shapes.
   */
  output_definition: ToolOutputDefinition;
}
/**
 * Stable per-tool rendering contract. Cheap to clone; embed in
 * any catalog or snapshot.
 */
export interface ToolOutputDefinition {
  /**
   * Tool name (matches `Tool::name()`).
   */
  name: string;
  /**
   * Render kind hint. Consumers (chat UI, TUI, replay log)
   * use this to pick the right renderer. Defaults to
   * [`RenderKind::Text`].
   */
  kind: RenderKind;
  /**
   * Optional title used by UI surfaces. Falls back to `name`
   * when absent.
   */
  title?: string | null;
  /**
   * Tool-authored hints (e.g. "this tool streams progress").
   * Read by UI surfaces; never interpreted by the LLM.
   */
  presentation: {
    [k: string]: unknown;
  };
  presentation_meta?: unknown;
}
/**
 * One component a UI row places in a slot.
 */
export interface UiComponent {
  /**
   * Which slot renders it, e.g. `chat.sidebar`.
   */
  slot: string;
  /**
   * The registered component kind. An unknown kind is carried
   * through unchanged and rendered as the fallback placeholder.
   */
  kind: string;
  props: unknown;
}
/**
 * One UI row's rendered description.
 */
export interface UiSchema {
  /**
   * The schema version the front-end renders. v1 is the current one.
   */
  version: number;
  /**
   * The panels the row contributes, in assembly order.
   */
  components: UiComponent[];
}
/**
 * `ui/schema` response body.
 */
export interface UiSchemaResponse {
  /**
   * The slots this deployment's `[ui]` rows declare, in declaration
   * order.
   */
  slots: UiSlot[];
  /**
   * The schemas of every `[ui]` row, in assembly order.
   */
  schemas: UiSchema[];
}
/**
 * The wire projection of one declared slot (`ui/schema`'s `slots`).
 */
export interface UiSlot {
  /**
   * The host-agnostic slot id.
   */
  id: string;
  /**
   * How many components the slot accepts.
   */
  cardinality: Cardinality;
  /**
   * The declaring `[ui]` row's id (`ui.`-namespaced).
   */
  owner: string;
}
/**
 * `PATCH /api/v1/schedules/{id}` body — pause / resume / mutate payload.
 */
export interface UpdateScheduleRequest {
  status?: JobStatus | null;
  payload?: unknown;
}

/**
 * Generic list response for all v1 list endpoints.
 *
 * See the module docs for the wire shape and field semantics.
 */
export interface List<T> {
  data: T[];
  next_cursor?: string | null;
  total?: number | null;
}
