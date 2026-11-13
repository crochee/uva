/**
 * Conversion helpers between the session-detail wire format
 * (`SessionDetail` / `SessionTurn` / `SessionArtifact`) and
 * the `ChatPage`'s `Message[]` shape persisted under
 * `uva.messages.<sessionId>`.
 *
 * The chat UI surfaces a conversation as a flat list of
 * `Message` objects, each with a `role` and a list of typed
 * `segments` (text, thinking, tool_block, ...). The session
 * detail endpoint persists the full conversation into
 * `session.history` as session events: user prompts
 * (role=user), agent text deltas, and tool_call / tool_result
 * events with `Part::data` payloads shaped like the provider's
 * `ToolUse` / `ToolResult` structs (`{id, name, input}` /
 * `{tool_use_id, content, is_error}`). The wire carries no
 * synthetic `kind` discriminator — we detect the segment kind
 * from the keys present in each `Part::data` payload.
 *
 * The durable log also carries the newer typed rows, and this
 * projection reads them too: an `assistant_message` row holds one
 * whole assistant turn (the provider `Message` — its text and its
 * tool calls together), and a `tool_result` row holds one tool's
 * outcome. Both project onto the same segments the legacy
 * `Model` rows produced, so a log written by either writer reads
 * the same way in the chat UI.
 *
 * Source priority:
 *   1. `session.history` — primary source of truth. Each
 *      transcript entry maps to one chat message (or a
 *      segment inside an in-flight assistant turn).
 *   2. `session.artifacts` — legacy fallback for sessions
 *      that were completed before history persistence landed.
 *      The artifact `metadata.kind` discriminator here is the
 *      legacy `tool_call` / `tool_result` marker; the wire
 *      shape is identical to the history path so the chat UI
 *      can render both without special-casing.
 *
 * The exported `seedChatFromSession` is the entry point a
 * caller uses to merge a reconstructed transcript
 * into whatever the local chat store already has, preferring
 * the existing entry (the user may have chatted in this
 * session and we'd rather not duplicate).
 */
import { contentText, extractImages, filesFromPersistedAttachments } from '../api/chat-message';
import { stripAttachmentSegments } from './strip-attachment-segments';
import type { ChatFile, ChatImage, CompactionInfo } from '../api/chat-message';
import type { SessionArtifact, SessionDetail, SessionTurn } from '../api/types';

export interface ChatMessageLike {
  id: string;
  role: 'user' | 'assistant' | 'subagent';
  /** Nesting depth of a `role: 'subagent'` message. */
  subagentDepth?: number;
  /** Per-child trace id grouping one delegated child's events. */
  subagentKey?: string;
  segments: ChatSegmentLike[];
  sessionId?: string;
  status?: string;
}

// Internal type used only within this module;
// `export` removed during the 2026-08-15 optimization pass
// (knip flagged as unused export).
interface ChatSegmentLike {
  id: string;
  type:
    | 'text'
    | 'thinking'
    | 'tool_call'
    | 'tool_result'
    | 'tool_block'
    | 'progress'
    | 'notice'
    | 'compaction';
  content: string;
  /** When type === 'compaction': the summary and the span it
   *  shadowed, rendered by `CompactionMarker`. */
  compaction?: CompactionInfo;
  toolName?: string;
  callContent?: string;
  resultContent?: string;
  /** Image parts carried by this segment, ready to render. A
   *  tool result whose content is `[text, image]` keeps its text
   *  in `resultContent` and its images here; a user prompt that
   *  sent a picture carries them on its text segment. */
  images?: ChatImage[];
  /** Attachments the user's turn carried, ready to render as
   *  chips. The live path fills this from the composer's pending
   *  attachments; the rebuild fills it from the references the
   *  persisted `UserInput` row carries (see
   *  `filesFromPersistedAttachments`), so a file-only turn shows
   *  a labelled chip instead of an empty bubble. */
  files?: ChatFile[];
  toolPending?: boolean;
  /** True when the matching tool_result carried
   *  `is_error: true`. The chat-style renderer paints the
   *  result sub-block red so the user can tell a failing
   *  tool from a successful one. */
  toolError?: boolean;
  /** `Part::data.id` from the matching tool_call — the
   *  pairing key that lets a `tool_result` find its own
   *  `tool_block` when two blocks are open at once (parallel
   *  or near-parallel tool calls in the same assistant turn). */
  toolUseId?: string;
}

/** Event type discriminators carried on each durable
 *  `SessionTurn` envelope (see `SessionTurn` in
 *  `src/api/types.ts` and the `event-durability-classification`
 *  spec). New history messages use these tag values; legacy
 *  `Artifact`s used the natural-shape detection the
 *  `chat-stream` module still owns. */
const EVENT_USER_INPUT = 'UserInput';
const EVENT_MODEL = 'Model';
/** Durable row for one assistant turn: `data` is a whole provider
 *  `Message` — the turn's text and its tool calls together — so the
 *  row projects onto ONE assistant message. */
const EVENT_ASSISTANT_MESSAGE = 'assistant_message';
/** Durable row for one tool outcome: `data` is the tool message the
 *  run committed — the tool's output plus the id of the call it
 *  answers. */
const EVENT_TOOL_RESULT = 'tool_result';
/** Recursive sub-agent trace envelope: `data` is
 *  `[AgentMeta, inner AgentEvent]`. Durable inner content is
 *  persisted verbatim, so history replay reconstructs
 *  delegated turns the same way the live stream shows them. */
const EVENT_AGENT = 'Agent';
/** Delegation markers the sub-agent session router writes into
 *  the PARENT's log: `subagent_enter` names the child session and
 *  peer agent, `subagent_exit` carries the terminal status. The
 *  child's own content lives in its own session — the parent
 *  keeps the link, not the trace. */
const EVENT_SUBAGENT_ENTER = 'subagent_enter';
const EVENT_SUBAGENT_EXIT = 'subagent_exit';
/** A committed compaction: `data.summary` is the replacement text and
 *  `surface_op` names the span it shadowed. It produces a timeline
 *  marker, not a replace: the reader can still see what was compacted
 *  away and why older turns stopped being sent. */
const EVENT_COMPACTION = 'compaction';
/** One durable plugin row: `kind` is the plugin's own event name and
 *  `payload` its structured body. Rendered as a notice — the row is
 *  provenance, not a model turn. */
const EVENT_PLUGIN = 'plugin';

/** Discriminators on the inner `ContentPart` carried by
 *  `Model` events. */
const PART_TEXT = 'text';
/** Model reasoning. Durable rows carry it, so a rebuilt transcript can
 *  show the same scratchpad the live projection streamed. */
const PART_REASONING = 'reasoning';
const PART_TOOL_USE = 'tool_use';
const PART_TOOL_RESULT = 'tool_result';

/** Legacy discriminator carried on the `metadata` of pre-history
 *  `Artifact`s. Only sessions completed before history
 *  persistence landed still hit this code path; new history
 *  messages use the `Part` discriminators above. */
const LEGACY_ARTIFACT_KIND_TOOL_CALL = 'tool_call';
const LEGACY_ARTIFACT_KIND_TOOL_RESULT = 'tool_result';

/**
 * Detect `Part::text` that the LLM emitted as an echo of a
 * tool event in SSE wire format. Some models occasionally
 * produce a stream-style prefix (`data: `) followed by a
 * JSON payload that mirrors the same shape as a real
 * `Part::data` — but encoded as plain text. Two
 * natural-shape variants appear in practice:
 *
 *   - tool_use echo: `{ "id": "...", "input": {...} }` (often
 *     without `name`, since the LLM is parroting a
 *     less-formatted wire form)
 *   - tool_result echo: `{ "content": "...", "is_error": true }`
 *     (often without `tool_use_id`, for the same reason)
 *
 * The echo is redundant — the same event is already on the
 * wire as a real `Part::data` and will be rendered as a
 * proper tool block, so we suppress the text rendering. This
 * is a presentation rule only; the data is still in
 * `Session.history` for inspection.
 *
 * Heuristic: trim leading whitespace and any single
 * `data: ` SSE prefix, then attempt a `JSON.parse`. The
 * `JSON.parse` is what makes the check robust — it rejects
 * any non-JSON prose, even if it happens to contain the
 * literal strings `"id"` and `"input"`.
 *
 * Exported so the chat-side reconstruction
 * (`historyToChatMessages`) applies the same rule everywhere.
 *
 * Logging: every key branch emits a `console.debug` line so
 * future investigations can trace exactly why a given
 * `Part::text` was treated as an echo (or not). The text
 * payload is truncated to 200 chars in the log to keep the
 * console readable. Flip via the `echo` namespace — the
 * default is silent; future debug sessions can opt in
 * without code changes.
 */
export function isToolEchoText(text: string): boolean {
  // Early-exit on the common path: the average `Part::text`
  // is plain prose (the assistant's response), not a JSON
  // echo. Logging here would flood the console, so we
  // gate the per-call log on the *negative* branches — the
  // moments when the function actually inspects the text.
  // The two positive branches log "echo detected" with the
  // matched variant.
  const trimmed = text.trim();
  const stripped = trimmed.startsWith('data:')
    ? trimmed.slice('data:'.length).replace(/^\s+/, '')
    : trimmed;
  // Branch 1: the SSE `data:` prefix was stripped. Log the
  // before/after so we can see what the LLM actually
  // produced vs. what we ran JSON.parse against.
  if (trimmed.startsWith('data:')) {
    console.debug('[isToolEchoText] stripped SSE `data:` prefix', {
      before: trimmed.slice(0, 200),
      after: stripped.slice(0, 200),
    });
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(stripped);
  } catch (err) {
    // Branch 2: the input didn't parse as JSON. This is
    // the most common path — the assistant's text is
    // plain prose, not a wire-format echo. Log at debug
    // level so a future investigation can opt in
    // (`console.debug` is off by default in production).
    console.debug('[isToolEchoText] not a tool echo — JSON.parse failed', {
      text: trimmed.slice(0, 200),
      error: err instanceof Error ? err.message : String(err),
    });
    return false;
  }
  // Branch 3: parsed value is not a plain object (null,
  // primitive, array). Reject — neither tool_use nor
  // tool_result is shaped this way on the wire.
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    console.debug('[isToolEchoText] not a tool echo — parsed value is not an object', {
      text: trimmed.slice(0, 200),
      parsedType: Array.isArray(parsed) ? 'array' : typeof parsed,
    });
    return false;
  }
  const obj = parsed as Record<string, unknown>;
  // tool_use echo: { id, input } (with or without name)
  if (typeof obj.id === 'string' && 'input' in obj) {
    // Branch 4: matched a tool_use shape. This is the
    // "duplicate data" case the user observed on the
    // session detail page — log the id so we can correlate
    // with the real `Part::data({id, name, input})` event
    // already on the wire.
    console.debug('[isToolEchoText] ECHO DETECTED — tool_use shape matched', {
      toolUseId: obj.id,
      hasName: typeof obj.name === 'string',
      text: trimmed.slice(0, 200),
    });
    return true;
  }
  // tool_result echo: { content, ... } (with or without
  // tool_use_id and is_error).
  if (typeof obj.content !== 'undefined') {
    // Branch 5: matched a tool_result shape. Same
    // rationale as branch 4 — this is the LLM echoing
    // the tool outcome as text, redundant against the
    // real `Part::data({tool_use_id, content, is_error})`
    // event. Log the tool_use_id if present so we can
    // correlate.
    console.debug('[isToolEchoText] ECHO DETECTED — tool_result shape matched', {
      toolUseId: typeof obj.tool_use_id === 'string' ? obj.tool_use_id : null,
      isError: obj.is_error === true,
      text: trimmed.slice(0, 200),
    });
    return true;
  }
  // Branch 6: parsed as JSON object but neither tool_use
  // nor tool_result shape. Common for arbitrary structured
  // output that the LLM emits — not a duplicate tool turn,
  // render as text.
  console.debug(
    '[isToolEchoText] not a tool echo — JSON object missing both tool_use and tool_result keys',
    {
      keys: Object.keys(obj),
      text: trimmed.slice(0, 200),
    },
  );
  return false;
}

/** Find the assistant message a no-target parent segment should attach
 *  to, returning a copy this fold may mutate. Sub-agent bubbles
 *  interleave after the assistant turn, so "trailing message" is
 *  ambiguous once delegation exists — parent content must always land
 *  on the assistant turn, never inside a child bubble.
 *
 *  The scan stops at a user message: an assistant turn owns only the
 *  content that follows it, so text arriving after a new user prompt
 *  belongs to a NEW turn. Reaching across the prompt merged two
 *  distinct answers into one bubble and placed it before the prompt
 *  that produced the second half. A log with a rerun (user, answer,
 *  user, answer) is the everyday case: it must fold to four messages,
 *  not three with the answers concatenated. */
function mutableAssistantTarget(
  messages: ChatMessageLike[],
  target?: ChatMessageLike,
): ChatMessageLike | undefined {
  // A `target` is a bubble this fold created for the row it is
  // applying (the sub-agent path); nothing else holds it, so it can
  // be written in place.
  if (target) return target;
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message.role === 'user') return undefined;
    if (message.role !== 'assistant') continue;
    // A compaction marker is an assistant-shaped divider, not an
    // answer. Appending into it would fold the next steps into the
    // marker (and make their text look like the turn's whole durable
    // prefix), so content after a marker starts a new bubble.
    if (!carriesModelContent(message)) return undefined;
    // The caller's projection is React state, and StrictMode
    // double-invokes reducers in development to surface impure
    // ones. Appending to the stored message object would therefore
    // apply the row twice — the UI showed a streamed step's text
    // and reasoning duplicated inside one assistant block until a
    // reload rebuilt the window from the log. Copy the message and
    // its segment list so the fold never touches the old state.
    const copy: ChatMessageLike = { ...message, segments: message.segments.slice() };
    messages[i] = copy;
    return copy;
  }
  return undefined;
}

/**
 * True when an assistant message renders model-visible content — text,
 * reasoning, a tool call or a tool result.
 *
 * `role: 'assistant'` alone is not enough: a compaction marker is
 * assistant-shaped but only carries a `compaction` segment, and
 * sub-agent bubbles carry child content rather than the parent turn's.
 * Both can sit between the parent assistant messages of one run, so the
 * live-prefix scan and the fold's append target must look through them
 * instead of treating them as the turn's durable answer.
 */
export function carriesModelContent(message: ChatMessageLike): boolean {
  if (message.role !== 'assistant') return false;
  return message.segments.some(
    (segment) =>
      segment.type === 'text' ||
      segment.type === 'thinking' ||
      segment.type === 'tool_block' ||
      segment.type === 'tool_result',
  );
}

/**
 * Append a text segment to an in-progress assistant turn,
 * coalescing with the most recent text segment when one
 * already exists. Mirrors the append rules used by the live
 * chat stream so the reconstructed transcript renders the same
 * way.
 */
function appendAgentSegment(
  messages: ChatMessageLike[],
  text: string,
  kind: 'text' | 'thinking',
  sessionId: string,
  target?: ChatMessageLike,
): void {
  if (!text) return;
  const owner = mutableAssistantTarget(messages, target);
  if (owner) {
    const segments = owner.segments;
    const tail = segments[segments.length - 1];
    if (tail && tail.type === kind) {
      segments[segments.length - 1] = {
        ...tail,
        content: tail.content + text,
      };
      return;
    }
    segments.push({
      id: crypto.randomUUID(),
      type: kind,
      content: text,
    });
    return;
  }
  if (target) return;
  messages.push({
    id: crypto.randomUUID(),
    role: 'assistant',
    segments: [{ id: crypto.randomUUID(), type: kind, content: text }],
    sessionId,
    status: 'completed',
  });
}

/**
 * Append a `tool_call` event as a fresh `tool_block` segment on
 * the trailing assistant turn (or open a new one). The block
 * starts pending; a following `tool_result` event will fill in
 * the result side. If the trailing turn already has a pending
 * tool_block — e.g. a tool_result that arrived before its call
 * — we still open a fresh block rather than corrupt the prior
 * pair's ordering.
 *
 * Carries `Part::data.id` forward as `toolUseId` so the
 * matching `tool_result` can attach to this exact block when
 * the assistant turn has more than one open call at once
 * (the live chat streamer's pairing key — see
 * `ChatPage::findPendingToolBlockIndex`).
 */
function appendToolCall(
  messages: ChatMessageLike[],
  payload: Record<string, unknown>,
  sessionId: string,
  target?: ChatMessageLike,
): void {
  const toolName = typeof payload.name === 'string' ? payload.name : undefined;
  const input = payload.input;
  const callBody =
    input !== undefined
      ? (() => {
          try {
            return JSON.stringify(input, null, 2);
          } catch {
            return String(input);
          }
        })()
      : '';
  const toolUseId = typeof payload.id === 'string' ? payload.id : undefined;
  const segment: ChatSegmentLike = {
    id: crypto.randomUUID(),
    type: 'tool_block',
    content: '',
    toolName,
    callContent: callBody,
    toolPending: true,
    toolUseId,
  };
  const owner = mutableAssistantTarget(messages, target);
  if (owner) {
    owner.segments.push(segment);
    return;
  }
  if (target) return;
  messages.push({
    id: crypto.randomUUID(),
    role: 'assistant',
    segments: [segment],
    sessionId,
    status: 'completed',
  });
}

/**
 * Append a `tool_result` event. When the trailing assistant
 * turn has a still-pending `tool_block` (i.e. we have an
 * in-flight call), we attach the result content to that block
 * and clear `toolPending`. Otherwise we emit a free-standing
 * `tool_result` segment on the trailing assistant turn (or open
 * a new one) — matches the chat UI's behaviour for tool results
 * whose call was never persisted (e.g. recovered mid-stream).
 *
 * Pairing rule: when the result carries a `tool_use_id`, only
 * blocks whose `toolUseId` matches exactly are considered —
 * the trailing-pending heuristic is reserved for the fallback
 * case (no id on the wire, or id mismatch) so two in-flight
 * tool calls each get their own result.
 */
function appendToolResult(
  messages: ChatMessageLike[],
  payload: Record<string, unknown>,
  sessionId: string,
  target?: ChatMessageLike,
): void {
  const content = contentText(payload.content);
  // Normalise to `undefined` when there are none: the three push
  // sites below must produce one shape, otherwise a rebuilt segment
  // differs from a freshly-created one purely by an empty array.
  const found = extractImages(payload.content);
  const images = found.length > 0 ? found : undefined;
  const toolName = typeof payload.tool_name === 'string' ? payload.tool_name : undefined;
  const isError = payload.is_error === true;
  const toolUseId = typeof payload.tool_use_id === 'string' ? payload.tool_use_id : undefined;

  const owner = mutableAssistantTarget(messages, target);
  if (owner) {
    const segments = owner.segments;
    let matchIdx = -1;
    let fallback = -1;
    for (let i = 0; i < segments.length; i++) {
      const seg = segments[i];
      if (seg.type !== 'tool_block' || seg.toolPending !== true) continue;
      if (toolUseId !== undefined && seg.toolUseId === toolUseId) {
        matchIdx = i;
        break;
      }
      // Last-resort fallback: the trailing still-pending
      // block. Only used when no exact id match exists —
      // keeps the legacy behaviour for transcripts that
      // don't carry `tool_use_id` (older wire versions).
      fallback = i;
    }
    if (matchIdx === -1) matchIdx = fallback;
    if (matchIdx !== -1) {
      segments[matchIdx] = {
        ...segments[matchIdx],
        resultContent: content,
        // The result's own images win; otherwise keep whatever
        // the block already had (a `tool_result` echo carries none).
        images: images ?? segments[matchIdx].images,
        toolPending: false,
        toolError: isError,
      };
      return;
    }
    segments.push({
      id: crypto.randomUUID(),
      type: 'tool_result',
      content,
      images,
      toolName,
      toolError: isError,
    });
    return;
  }
  if (target) return;
  messages.push({
    id: crypto.randomUUID(),
    role: 'assistant',
    segments: [
      {
        id: crypto.randomUUID(),
        type: 'tool_result',
        content,
        images,
        toolName,
        toolError: isError,
      },
    ],
    sessionId,
    status: 'completed',
  });
}

/**
 * The `content` parts of a provider `Message` payload.
 *
 * The wire form is the serde shape of `uva_provider::Content`:
 * an externally tagged enum, so one part arrives as
 * `{"Single": <part>}` and several as `{"Multi": [<part>, …]}`.
 * The loose spelling — `{"role": …, "content": [<part>, …]}` — is
 * read too, so a row written by hand and a row written by the
 * backend project the same way. Those are the two shapes the
 * server's own fold accepts, so this reader and that fold agree on
 * what a valid row is.
 *
 * Exported so the session detail page counts the parts of a row
 * with the same reader this projection uses.
 */
export function messageParts(payload: unknown): Record<string, unknown>[] {
  const message = toRecord(payload);
  if (!message) return [];
  const content = message.content;
  if (Array.isArray(content)) {
    return content.map(toRecord).filter((part): part is Record<string, unknown> => part !== null);
  }
  const obj = toRecord(content);
  if (!obj) return [];
  if (Array.isArray(obj.Multi)) {
    return obj.Multi.map(toRecord).filter((part): part is Record<string, unknown> => part !== null);
  }
  const single = toRecord(obj.Single);
  return single ? [single] : [];
}

/**
 * Dispatch one `Model`-envelope payload (`data` = a single
 * `ContentPart`) onto the message list. When `target` is
 * provided the segment lands in that message (used for
 * sub-agent bubbles); otherwise the trailing assistant turn
 * absorbs it, matching the live-stream reducer.
 */
function applyContentPart(
  data: Record<string, unknown>,
  sessionId: string,
  out: ChatMessageLike[],
  target?: ChatMessageLike,
): void {
  const kind = typeof data.type === 'string' ? data.type : '';
  if (kind === PART_TOOL_USE) {
    appendToolCall(out, data, sessionId, target);
    return;
  }
  if (kind === PART_TOOL_RESULT) {
    appendToolResult(out, data, sessionId, target);
    return;
  }
  if (kind === PART_TEXT) {
    const text = typeof data.text === 'string' ? data.text : '';
    if (!text) return;
    // Skip the LLM's text echo of a tool event. The real
    // event is already on the wire as a typed `ContentPart`
    // and will be rendered as a tool block; the text echo
    // would otherwise append the same data to the
    // assistant's text segment, creating the appearance
    // of a duplicate tool turn.
    if (isToolEchoText(text)) return;
    appendAgentSegment(out, text, 'text', sessionId, target);
    return;
  }
  if (kind === PART_REASONING) {
    const text = typeof data.text === 'string' ? data.text : '';
    appendAgentSegment(out, text, 'thinking', sessionId, target);
  }
}

/**
 * Project one `tool_result` row onto the message list.
 *
 * Two rows carry this type. The run's own row holds the tool
 * message the loop committed: the provider `Message` with
 * `role: "tool"`, the tool's output as its `content`, and the id of
 * the call it answers in `tool_call_id`. The crash-repair closer
 * holds a marker instead — `call_id`, the tool's name, an `error`
 * code and `interrupted: true` — because the call never finished.
 * Both name the call, and that id is the pairing key the assistant
 * turn's `tool_use` part carried, so the result lands on that
 * call's block — the same pairing the live stream does.
 *
 * A row whose content is a `tool_result` part already carries the
 * whole payload (the body, the error flag). The harness's own row
 * carries the tool's output as plain parts instead, so the payload
 * is rebuilt from the message.
 */
function applyToolResultMessage(
  entry: SessionTurn,
  sessionId: string,
  out: ChatMessageLike[],
): void {
  const data = toRecord(entry.data);
  if (!data) return;
  const parts = messageParts(data);
  const resultPart = parts.find((part) => part.type === PART_TOOL_RESULT);
  if (resultPart) {
    // The part carries its own failure flag; the row's flag says the
    // same thing from the row's side, so either may set it.
    const payload = entry.is_error === true ? { ...resultPart, is_error: true } : resultPart;
    appendToolResult(out, payload, sessionId);
    return;
  }
  const toolUseId =
    typeof data.tool_call_id === 'string'
      ? data.tool_call_id
      : typeof data.call_id === 'string'
        ? data.call_id
        : '';
  if (!toolUseId) return;
  // A failed tool says so three ways: the row's own flag (the run's
  // row), the repair closer's error code, and the closer's
  // `interrupted` marker. A call that never finished must not read
  // as one that returned.
  const errorCode = typeof data.error === 'string' ? data.error : '';
  const isError = entry.is_error === true || errorCode !== '' || data.interrupted === true;
  const toolName =
    typeof data.tool_name === 'string'
      ? data.tool_name
      : typeof data.name === 'string'
        ? data.name
        : undefined;
  appendToolResult(
    out,
    {
      type: PART_TOOL_RESULT,
      tool_use_id: toolUseId,
      // The repair closer carries no output — its error code is the
      // only thing it knows about the call — so the code is what
      // the block shows.
      content: parts.length > 0 ? parts : errorCode ? [{ type: PART_TEXT, text: errorCode }] : [],
      tool_name: toolName,
      is_error: isError,
    },
    sessionId,
  );
}

/**
 * Convert one history entry into chat messages. A `UserInput`
 * envelope produces one user message — its text plus one chip per
 * attachment reference the row carries; `Model` envelopes dispatch
 * on the inner `ContentPart.type` — text appends to the trailing
 * assistant turn (coalesced), `tool_use` / `tool_result` drive
 * the tool-block pairing logic above.
 *
 * Each `Model` envelope carries exactly one `ContentPart` on the
 * wire (the controller writes one event per agent frame), so a
 * single dispatch is enough — no inner part loop is needed.
 *
 * The typed rows carry whole messages instead: an
 * `assistant_message` envelope is one assistant turn (its text and
 * its tool calls together), and a `tool_result` envelope is one
 * tool's outcome. Both land on the same segments the `Model`
 * envelopes produced.
 *
 * `Agent` envelopes (`data` = `[AgentMeta, inner AgentEvent]`)
 * reconstruct the delegated child's durable content into its own
 * `role: 'subagent'` bubble, mirroring the live stream's
 * sub-agent rendering. Inner `System` / `ModelDone` frames are
 * lifecycle noise and were never persisted as durable content,
 * so only nested `Model` parts are dispatched.
 */
function applyHistoryEntry(entry: SessionTurn, sessionId: string, out: ChatMessageLike[]): void {
  const envelope = entry.type;
  const data = entry.data;

  if (envelope === EVENT_USER_INPUT) {
    const obj = toRecord(data);
    // A `UserInput` row is the controller's prompt record
    // (`{"text": …}`). The session controller deliberately does
    // NOT persist user image/audio/document BYTES — a
    // multi-megabyte `data:` URI per turn would bloat the JSONL
    // log — so a turn that carried attachments writes
    // `data.attachments` beside the text: one reference per
    // attachment (`{kind, name, mime_type, byte_len}`), which is
    // everything a chip needs and nothing more. Absent for a
    // text-only turn, and absent entirely in logs written before
    // the references landed.
    const text = obj && typeof obj.text === 'string' ? obj.text : '';
    const files = obj ? filesFromPersistedAttachments(obj.attachments) : [];
    // A file-only send writes a row with no text; it must still
    // produce the bubble, otherwise the turn reads as if the user
    // sent nothing at all.
    if (!text && files.length === 0) return;
    out.push({
      id: crypto.randomUUID(),
      role: 'user',
      segments: [
        {
          id: crypto.randomUUID(),
          type: 'text',
          content: text,
          files: files.length > 0 ? files : undefined,
        },
      ],
    });
    return;
  }

  if (envelope === EVENT_COMPACTION) {
    const obj = toRecord(data);
    const summary = obj && typeof obj.summary === 'string' ? obj.summary : '';
    const shadowedTokens =
      obj && typeof obj.shadowed_token_count === 'number' ? obj.shadowed_token_count : undefined;
    const op = entry.surface_op;
    out.push({
      id: crypto.randomUUID(),
      role: 'assistant',
      segments: [
        {
          id: crypto.randomUUID(),
          type: 'compaction',
          content: summary,
          compaction: {
            summary,
            ...(op?.start !== undefined ? { start: op.start } : {}),
            ...(op?.end !== undefined ? { end: op.end } : {}),
            ...(op?.start !== undefined && op?.end !== undefined
              ? { shadowedMessages: op.end - op.start }
              : {}),
            ...(shadowedTokens !== undefined ? { shadowedTokens } : {}),
            ...(op?.source_event_seqs && op.source_event_seqs.length > 0
              ? {
                  firstSeq: Math.min(...op.source_event_seqs),
                  lastSeq: Math.max(...op.source_event_seqs),
                }
              : {}),
          },
        },
      ],
      sessionId,
      status: 'completed',
    });
    return;
  }

  if (envelope === EVENT_PLUGIN) {
    const obj = toRecord(data);
    const kind = obj && typeof obj.kind === 'string' && obj.kind ? obj.kind : 'unnamed';
    const payload = obj && obj.payload !== undefined ? JSON.stringify(obj.payload) : '';
    out.push({
      id: crypto.randomUUID(),
      role: 'assistant',
      segments: [
        {
          id: crypto.randomUUID(),
          type: 'notice',
          content: payload
            ? `插件事件 **${kind}** · \`${payload.slice(0, 200)}\``
            : `插件事件 **${kind}**`,
        },
      ],
      sessionId,
      status: 'completed',
    });
    return;
  }

  if (envelope === EVENT_SUBAGENT_ENTER) {
    const obj = toRecord(data);
    const childId = typeof obj?.child_session_id === 'string' ? obj.child_session_id : null;
    const agent = typeof obj?.agent_name === 'string' ? obj.agent_name : 'unknown';
    const depth = typeof obj?.depth === 'number' ? obj.depth : 1;
    const link = childId ? ` [查看子会话 →](/chat/${childId})` : '';
    out.push({
      id: crypto.randomUUID(),
      role: 'subagent',
      subagentDepth: depth,
      subagentKey: childId ?? undefined,
      segments: [
        {
          id: crypto.randomUUID(),
          type: 'notice',
          content: `已委派子代理 **${agent}**。${link}`,
        },
      ],
    });
    return;
  }

  if (envelope === EVENT_SUBAGENT_EXIT) {
    const obj = toRecord(data);
    const childId = typeof obj?.child_session_id === 'string' ? obj.child_session_id : null;
    const status = typeof obj?.status === 'string' ? obj.status : 'completed';
    const label = status === 'completed' ? '已完成' : status === 'cancelled' ? '已取消' : '未完成';
    const link = childId ? ` [查看子会话 →](/chat/${childId})` : '';
    out.push({
      id: crypto.randomUUID(),
      role: 'subagent',
      subagentKey: childId ?? undefined,
      segments: [
        {
          id: crypto.randomUUID(),
          type: 'notice',
          content: `子代理${label}。${link}`,
        },
      ],
    });
    return;
  }

  if (envelope === EVENT_AGENT) {
    // `data` = `[AgentMeta, inner AgentEvent]`; nested `Agent`
    // payloads (a sub-agent that itself delegated) are unwrapped
    // to the innermost non-Agent event so the deepest durable
    // content lands on the innermost child's bubble.
    if (!Array.isArray(data) || data.length < 2) return;
    let meta = data[0] as Record<string, unknown> | undefined;
    let inner: unknown = data[1];
    while (
      inner &&
      typeof inner === 'object' &&
      !Array.isArray(inner) &&
      (inner as Record<string, unknown>).type === EVENT_AGENT
    ) {
      const nested = (inner as Record<string, unknown>).data;
      if (!Array.isArray(nested) || nested.length < 2) {
        inner = undefined;
        break;
      }
      meta = nested[0] as Record<string, unknown>;
      inner = nested[1];
    }
    if (!inner || typeof inner !== 'object' || Array.isArray(inner)) return;
    const innerRecord = inner as Record<string, unknown>;
    if (innerRecord.type !== EVENT_MODEL) return;
    const part = toRecord(innerRecord.data);
    if (!part) return;
    const metaRecord = meta ?? {};
    const childId =
      typeof metaRecord.child_session_id === 'string' && metaRecord.child_session_id.length > 0
        ? metaRecord.child_session_id
        : undefined;
    const depth = typeof metaRecord.parent_depth === 'number' ? metaRecord.parent_depth : 1;
    const childKey = childId ?? `subagent:d${depth}`;
    // Silent skip: the parent transcript does not render subagent
    // content. The `task` tool's SubagentSummary (link card
    // mounted on the tool_block) is the user's only entry point
    // to the child from this transcript; the child's full trace
    // lives at `/chat/<childId>`, where the unwrapped log
    // goes through `EVENT_MODEL` and is rendered in full.
    //
    // The underlying event rows are still in the session log (not
    // deleted). To restore the embedded rendering in the future,
    // re-introduce `ensureSubagentTarget` and dispatch
    // `applyContentPart(part, sessionId, out, target)` again — it
    // is not a one-line revert because the target plumbing runs
    // through `appendAgentSegment` / `appendToolCall` /
    // `appendToolResult`.
    void childKey;
    return;
  }

  if (envelope === EVENT_ASSISTANT_MESSAGE) {
    // One assistant turn is one durable row: `data` is the whole
    // provider `Message`, so its parts expand into the turn's
    // segments — text into the text segment, `tool_use` into a
    // tool block. The parts go through the same dispatch the
    // legacy `Model` rows use, so the tool pairing rules live in
    // one place and the turn renders the way the live stream
    // rendered it.
    for (const part of messageParts(data)) {
      applyContentPart(part, sessionId, out);
    }
    return;
  }

  if (envelope === EVENT_TOOL_RESULT) {
    applyToolResultMessage(entry, sessionId, out);
    return;
  }

  if (envelope !== EVENT_MODEL) return;
  const obj = toRecord(data);
  if (!obj) return;
  applyContentPart(obj, sessionId, out);
}

/** Narrow the union `data` shape to a JSON object, or `null`. */
function toRecord(data: unknown): Record<string, unknown> | null {
  return data !== null && typeof data === 'object' && !Array.isArray(data)
    ? (data as Record<string, unknown>)
    : null;
}

/**
 * A fold of durable rows into the chat timeline, plus the log ordinal
 * each message came from.
 *
 * `sources[i]` is the durable ordinal of the row that created
 * `messages[i]`; a replace op locates what it shadows by that ordinal,
 * never by a UI index.
 */
export interface RowProjection {
  sessionId: string;
  messages: ChatMessageLike[];
  sources: number[];
}

/** An empty projection for one session's window. */
export function newRowProjection(sessionId: string): RowProjection {
  return { sessionId, messages: [], sources: [] };
}

/**
 * Fold durable rows onto a projection.
 *
 * Incremental by design: the chat page applies each `follow` row batch
 * to the projection it already holds, so a step boundary does not
 * re-allocate the ids of every earlier message (which would remount the
 * list and lose the reader's scroll position). A fresh window starts
 * from `newRowProjection(sessionId)` and passes the whole baseline.
 *
 * The output preserves the on-wire ordering of events, with one
 * exception: a `UserInput` row carrying a `surface_op` replace (the row
 * a regenerate persists) first drops the messages its cited seqs
 * shadow, so a rerun shows the replacement story — one prompt bubble,
 * the new answer. A `compaction` row produces a marker instead of a
 * replace, so the reader can still see the span that was compacted.
 */
export function projectRows(state: RowProjection, rows: ReadonlyArray<SessionTurn>): RowProjection {
  const messages = state.messages.slice();
  const sources = state.sources.slice();
  for (const entry of rows) {
    if (entry.type === EVENT_USER_INPUT) {
      dropReplacedTurn(entry, messages, sources);
    }
    const before = messages.length;
    applyHistoryEntry(entry, state.sessionId, messages);
    const ordinal =
      typeof entry.seq === 'number' ? entry.seq : (sources[sources.length - 1] ?? 0) + 1;
    for (let i = before; i < messages.length; i += 1) {
      sources[i] = ordinal;
    }
  }
  return { sessionId: state.sessionId, messages, sources };
}

/**
 * Remove the messages a replace op shadows, before the
 * replacing row's own message lands.
 *
 * The cited `source_event_seqs` name the shadowed surface rows;
 * the inclusive min..max range around them also catches messages
 * built from log-only rows inside the same turn (sub-agent
 * `Agent` envelopes the server's fold never indexes), so a
 * regenerated turn's delegation traces go with its answer.
 */
function dropReplacedTurn(entry: SessionTurn, out: ChatMessageLike[], sources: number[]): void {
  const seqs = entry.surface_op?.source_event_seqs;
  if (!seqs || seqs.length === 0) return;
  const lo = Math.min(...seqs);
  const hi = Math.max(...seqs);
  for (let i = out.length - 1; i >= 0; i -= 1) {
    const source = sources[i];
    if (source !== undefined && source >= lo && source <= hi) {
      out.splice(i, 1);
      sources.splice(i, 1);
    }
  }
}

/**
 * Group `tool_call` and `tool_result` artifacts by
 * `tool_use_id` and emit one assistant `tool_block` message per
 * pair. This is the legacy fallback path used when a session
 * has no persisted history — pre-`Session.history` runs only
 * had artifacts to reconstruct from. The legacy artifact
 * `metadata.kind` carries the explicit discriminator; new
 * history messages don't need it.
 */
function artifactsToAssistantMessage(
  artifacts: ReadonlyArray<SessionArtifact>,
  sessionId: string,
): ChatMessageLike | null {
  const groups = new Map<
    string,
    {
      toolUseId: string;
      toolName?: string;
      call?: SessionArtifact;
      result?: SessionArtifact;
      isError?: boolean;
    }
  >();
  const loose: SessionArtifact[] = [];

  for (const art of artifacts) {
    const kind = art.metadata?.kind;
    const toolUseId = art.metadata?.tool_use_id;
    if (
      (kind === LEGACY_ARTIFACT_KIND_TOOL_CALL || kind === LEGACY_ARTIFACT_KIND_TOOL_RESULT) &&
      typeof toolUseId === 'string'
    ) {
      let group = groups.get(toolUseId);
      if (!group) {
        group = { toolUseId, toolName: art.metadata?.tool_name };
        groups.set(toolUseId, group);
      }
      if (kind === LEGACY_ARTIFACT_KIND_TOOL_CALL) {
        group.call = art;
        if (art.metadata?.tool_name) group.toolName = art.metadata.tool_name;
      } else {
        group.result = art;
        if (art.metadata?.is_error) group.isError = true;
      }
    } else {
      loose.push(art);
    }
  }

  const segments: ChatSegmentLike[] = [];
  for (const group of groups.values()) {
    const callText = group.call ? extractArtifactText(group.call) : '';
    const resultText = group.result ? extractArtifactText(group.result) : '';
    segments.push({
      id: crypto.randomUUID(),
      type: 'tool_block',
      content: '',
      toolName: group.toolName,
      callContent: callText,
      resultContent: resultText,
      toolPending: false,
      toolError: group.isError,
    });
  }
  for (const art of loose) {
    segments.push({
      id: crypto.randomUUID(),
      type: 'text',
      content: extractArtifactText(art),
    });
  }

  if (segments.length === 0) return null;
  return {
    id: crypto.randomUUID(),
    role: 'assistant',
    segments,
    sessionId,
    status: 'completed',
  };
}

function extractArtifactText(art: SessionArtifact): string {
  if (!art.parts) return '';
  return art.parts.map((p) => (typeof p.text === 'string' ? p.text : '')).join('');
}

/**
 * Build the messages that best represent the session's history
 * for the chat UI. Prefers `session.history`; falls back to
 * `session.artifacts` when the server has not yet persisted
 * history (legacy sessions).
 */
export function reconstructMessagesFromSession(session: SessionDetail): ChatMessageLike[] {
  if (session.history && session.history.length > 0) {
    return projectRows(newRowProjection(session.id), session.history).messages;
  }
  const assistant = artifactsToAssistantMessage(session.artifacts, session.id);
  return assistant ? [assistant] : [];
}

/**
 * Merge a list of reconstructed messages into an existing chat
 * store.
 *
 * Dedup rules (in order):
 *   1. If the existing store already contains a user message
 *      with the same text as the first user message in the
 *      reconstructed list, the chat is already synced with
 *      this session — return the existing store unchanged.
 *      This catches the common case where the user has
 *      chatted in this session, then clicks "Continue chat" on
 *      the session detail page; seeding would otherwise add
 *      a duplicate user prompt. The check is "any user
 *      message in the existing list" (not "first one")
 *      because a user may have multiple sessions in the same
 *      thread — only the matching prompt indicates the
 *      session is already in the chat.
 *   2. Otherwise, drop reconstructed messages whose
 *      `sessionId` is already represented in the existing
 *      store. `sessionId` is set on the assistant turn that
 *      produced a streamed agent event; matches mean
 *      "this session's transcript is already in the chat".
 *   3. Reconstructed messages WITHOUT a `sessionId` (i.e.
 *      the user prompt reconstructed from
 *      `Session.history`) are appended — they have no
 *      `sessionId`-keyed overlap risk, and rule 1 already
 *      ruled out the same prompt being present.
 *
 * Returns the merged array. Pure function so callers can apply
 * it and then persist the result.
 */
export function mergeReconstructedMessages(
  existing: ReadonlyArray<ChatMessageLike>,
  reconstructed: ReadonlyArray<ChatMessageLike>,
): ChatMessageLike[] {
  if (reconstructed.length === 0) return existing.slice();

  // (1) Chat already in sync with this session?
  const reconstructedFirstUserText = firstUserPromptText(reconstructed);
  if (reconstructedFirstUserText !== null) {
    if (existing.some((m) => userPromptText(m) === reconstructedFirstUserText)) {
      return existing.slice();
    }
  }

  // (2) Drop reconstructed messages whose sessionId is
  // already in the existing store.
  const existingSessionIds = new Set(
    existing.map((m) => m.sessionId).filter((id): id is string => Boolean(id)),
  );
  const fresh = reconstructed.filter((m) => !m.sessionId || !existingSessionIds.has(m.sessionId));
  if (fresh.length === 0) return existing.slice();
  return [...existing, ...fresh];
}

/**
 * Extract the user-prompt text of a single message. Returns
 * `''` for non-user messages or user messages with no text
 * segments (e.g. an empty `progress` placeholder).
 */
function userPromptText(m: ChatMessageLike): string {
  if (m.role !== 'user') return '';
  return m.segments
    .filter((s) => s.type === 'text')
    .map((s) => s.content)
    .join('');
}

/**
 * Extract the text of the first user message in a list. The
 * chat UI renders user prompts as a single `text` segment on a
 * `user`-role message; everything else (thinking segments,
 * empty segments) is irrelevant for "have we already seen
 * this prompt?" matching.
 *
 * Returns `null` if the list has no user message — caller
 * treats that as "no overlap check possible" and falls through
 * to the sessionId-based dedup.
 */
export function firstUserPromptText(messages: ReadonlyArray<ChatMessageLike>): string | null {
  for (const m of messages) {
    const text = userPromptText(m);
    if (text) return text;
  }
  return null;
}

/**
 * Convenience wrapper for the "Continue chat" click path.
 * Reads the current chat store under
 * `uva.messages.<sessionId>`, merges in the reconstructed
 * messages, and writes the result back. Safe to call when
 * nothing is stored yet — the store starts as an empty array
 * and is updated in place.
 */
export function seedChatFromSession(
  sessionId: string,
  session: SessionDetail,
  storageKeyPrefix = 'uva.messages.',
): boolean {
  if (typeof localStorage === 'undefined') return false;
  const key = `${storageKeyPrefix}${sessionId}`;
  const reconstructed = reconstructMessagesFromSession(session);
  if (reconstructed.length === 0) return false;
  let existing: ChatMessageLike[] = [];
  try {
    const raw = localStorage.getItem(key);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) existing = parsed as ChatMessageLike[];
    }
  } catch {
    existing = [];
  }
  const merged = mergeReconstructedMessages(existing, reconstructed);
  if (merged.length === existing.length) return false;
  try {
    // Reconstructed segments carry images (a tool result that
    // returned a picture). Persisting those base64 payloads would
    // bloat localStorage past its quota and silently kill the
    // write, so apply the same strip policy the chat page's own
    // persist effect uses. The pictures come back from history on
    // the next render.
    localStorage.setItem(key, JSON.stringify(stripAttachmentSegments(merged)));
    return true;
  } catch {
    return false;
  }
}
