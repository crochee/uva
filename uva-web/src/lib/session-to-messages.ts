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
import type { SessionArtifact, SessionDetail, SessionEventType, SessionTurn } from '../api/types';

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
  /**
   * The ordinal of the assistant row this message came from — that
   * row's `seq`, in decimal.
   *
   * The row's identity in the session log, and the only thing a
   * `chat/feedback` target can name: the server resolves the target
   * against the log's own rows. `undefined` when no assistant row
   * landed in this message with an ordinal of its own — the live
   * projection, and a bubble that only a tool result opened.
   */
  ordinal?: number;
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
    | 'warning'
    | 'hook_block'
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

/** Event type discriminators carried on each durable `SessionTurn`
 *  envelope. The typed vocabulary now arrives as the generated
 *  `SessionEventType` union (see `SessionTurn` in `src/api/types.ts`)
 *  and is accounted for in `TYPED_ROW_TREATMENT`; what stays here is
 *  the legacy envelope dialect, which predates that union and is
 *  read through `legacyRowType`. */
const EVENT_USER_INPUT = 'UserInput';
/** The typed turn record this build writes: `data` is the provider
 *  message the model saw, and the turn's attachment refs ride the
 *  row's own `attachments` field. */
const EVENT_USER_MESSAGE = 'user_message';
const EVENT_MODEL = 'Model';
/** The legacy recursive sub-agent trace envelope: `data` is
 *  `[child metadata, inner event]`. Durable inner content is
 *  persisted verbatim, so history replay reconstructs
 *  delegated turns the same way the live stream shows them.
 *  No build writes this envelope any more — a delegated child
 *  lives in its own session — but logs written before the
 *  delegation channel's retirement still carry it, so the fold
 *  keeps reading them. */
const EVENT_AGENT = 'Agent';

/** The `type` value of a durable row, widened past the typed
 *  vocabulary. Rows written before the typed writer carry the legacy
 *  envelope dialect (`UserInput` / `Model` / `Agent`) — strings
 *  outside `SessionEventType` — so a reader that still serves those
 *  logs compares through this widening instead of the closed union. */
export function legacyRowType(row: SessionTurn): string | null | undefined {
  return row.type;
}

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
 *
 * `ordinal` is the ordinal of the assistant row the text came from.
 * It is stamped on the message the text lands in, so the message
 * names the row it is about — the only thing a rating can name — and
 * only when the row is **answer-bearing**: a row whose only content
 * is reasoning renders no line in the TUI, so the TUI's
 * `resume_material` keeps the previous answer's ordinal there.
 */
function appendAgentSegment(
  messages: ChatMessageLike[],
  text: string,
  kind: 'text' | 'thinking',
  sessionId: string,
  target?: ChatMessageLike,
  ordinal?: number,
): void {
  if (!text) return;
  // Only an answer-bearing row names a rating target. A reasoning-only
  // row renders no line in the TUI, so `resume_material` keeps the
  // previous answer's ordinal there; a bubble whose only content is
  // reasoning therefore names no row at all — there is no answer to
  // rate.
  const answerOrdinal = kind === 'text' ? ordinal : undefined;
  const owner = mutableAssistantTarget(messages, target);
  if (owner) {
    // Last-wins over answer-bearing rows: a bubble that spans several
    // assistant rows is one message, and the ordinal it offers a
    // rating is the newest answer-bearing row it holds — the same row
    // the TUI's `resume_material` names, so one thumbs-up lands on the
    // same line in both surfaces.
    if (answerOrdinal !== undefined) {
      owner.ordinal = answerOrdinal;
    }
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
    ordinal: answerOrdinal,
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
  ordinal?: number,
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
    // Last-wins: a bubble that spans several assistant rows is one
    // message, and the ordinal it offers a rating is the newest row
    // it holds — the same row the TUI's `resume_material` names, so
    // one thumbs-up lands on the same line in both surfaces.
    if (ordinal !== undefined) {
      owner.ordinal = ordinal;
    }
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
    ordinal,
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
 *
 * `ordinal` is the ordinal of the assistant row the part came from;
 * it is stamped on the message the part lands in.
 */
function applyContentPart(
  data: Record<string, unknown>,
  sessionId: string,
  out: ChatMessageLike[],
  target?: ChatMessageLike,
  ordinal?: number,
): void {
  const kind = typeof data.type === 'string' ? data.type : '';
  if (kind === PART_TOOL_USE) {
    appendToolCall(out, data, sessionId, target, ordinal);
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
    appendAgentSegment(out, text, 'text', sessionId, target, ordinal);
    return;
  }
  if (kind === PART_REASONING) {
    const text = typeof data.text === 'string' ? data.text : '';
    appendAgentSegment(out, text, 'thinking', sessionId, target, ordinal);
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
 * Fold one `tool_output` row: a partial-output chunk of a running
 * tool call.
 *
 * This is the reload's copy of what the live projection streamed.
 * The row is log-only — it never reaches the model — but the
 * transcript reader uses it exactly as the live frames did: the
 * chunk extends the pending tool block's output. When the call's
 * `tool_result` row lands it replaces the preview with the settled
 * output, so a finished call reads as its committed result and a
 * call the process died in the middle of still shows how far it
 * got.
 */
function applyToolOutputRow(entry: SessionTurn, out: ChatMessageLike[]): void {
  const data = toRecord(entry.data);
  const rawOutput = data ? data.output : undefined;
  const chunk = typeof rawOutput === 'string' ? rawOutput : '';
  if (!chunk) return;
  const callId = entry.call_id ?? undefined;
  const owner = mutableAssistantTarget(out);
  if (!owner) return;
  let matchIdx = -1;
  let fallback = -1;
  for (let i = 0; i < owner.segments.length; i += 1) {
    const segment = owner.segments[i];
    if (segment.type !== 'tool_block' || segment.toolPending !== true) continue;
    if (callId !== undefined && segment.toolUseId === callId) {
      matchIdx = i;
      break;
    }
    fallback = i;
  }
  if (matchIdx === -1) matchIdx = fallback;
  if (matchIdx === -1) return;
  const segment = owner.segments[matchIdx];
  owner.segments[matchIdx] = {
    ...segment,
    resultContent: `${segment.resultContent ?? ''}${chunk}`,
    // Still pending: the chunk is what the call has produced so far,
    // not its answer.
    toolPending: true,
  };
}

/**
 * How the projection folds one durable row.
 *
 * Two dialects reach this fold:
 *
 * - the **legacy envelope dialect** (`UserInput` / `Model` /
 *   `Agent` on `type`) predates the typed vocabulary; those rows
 *   are compared through [`legacyRowType`], because their strings
 *   are outside `SessionEventType`.
 * - the **typed vocabulary** — every tag the server can write — is
 *   accounted for in [`TYPED_ROW_TREATMENT`]: each tag either
 *   renders (a handler) or is ignored-by-design (`null`). The
 *   table is a `Record` over the generated `SessionEventType`
 *   union, so a tag added to the vocabulary fails `tsc` until the
 *   table says what to do with the row. Silence is never the
 *   default.
 *
 * A `UserInput` envelope produces one user message — its text plus
 * one chip per attachment reference the row carries; `Model`
 * envelopes dispatch on the inner `ContentPart.type` — text
 * appends to the trailing assistant turn (coalesced), `tool_use` /
 * `tool_result` drive the tool-block pairing logic above.
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
 * The legacy `Agent` envelope (`data` = `[child metadata, inner
 * event]`) reconstructs the delegated child's durable content
 * into its own `role: 'subagent'` bubble, mirroring the live
 * stream's sub-agent rendering. Inner `System` / `ModelDone`
 * frames are lifecycle noise and were never persisted as durable
 * content, so only nested `Model` parts are dispatched.
 */

/** Fold one legacy `UserInput` row (`data` = `{"text": …}`). */
function applyLegacyUserInputRow(entry: SessionTurn, out: ChatMessageLike[]): void {
  const obj = toRecord(entry.data);
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
}

/** Fold one legacy `Model` row (`data` = one `ContentPart`). */
function applyLegacyModelRow(entry: SessionTurn, sessionId: string, out: ChatMessageLike[]): void {
  const obj = toRecord(entry.data);
  if (!obj) return;
  applyContentPart(obj, sessionId, out);
}

/** Fold one legacy `Agent` row (`data` = `[child metadata, event]`). */
function applyLegacyAgentRow(entry: SessionTurn): void {
  // `data` = `[child metadata, inner event]`; nested `Agent`
  // payloads (a sub-agent that itself delegated) are unwrapped
  // to the innermost non-Agent event so the deepest durable
  // content lands on the innermost child's bubble.
  const data = entry.data;
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
}

/** Fold one `user_message` row: `data` is the provider message the
 *  controller wrote for the turn — its text in the content parts —
 *  and the attachment references ride the row's own `attachments`
 *  field rather than inside the payload. */
function applyUserMessageRow(entry: SessionTurn, out: ChatMessageLike[]): void {
  const text = messageParts(entry.data)
    .filter((part) => part.type === PART_TEXT && typeof part.text === 'string')
    .map((part) => (typeof part.text === 'string' ? part.text : ''))
    .join('');
  const files = filesFromPersistedAttachments(entry.attachments);
  // A file-only turn writes a row with no text; it must still
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
}

/** Fold one `assistant_message` row: `data` is the whole provider
 *  `Message`, so its parts expand into the turn's segments — text
 *  into the text segment, `tool_use` into a tool block. The parts
 *  go through the same dispatch the legacy `Model` rows use, so
 *  the tool pairing rules live in one place and the turn renders
 *  the way the live stream rendered it.
 *
 *  The row's ordinal rides along: it is the answer's identity in the
 *  session log, and the only thing a rating of that answer can
 *  name. A row without one (an old log) stamps nothing. */
function applyAssistantMessageRow(
  entry: SessionTurn,
  sessionId: string,
  out: ChatMessageLike[],
): void {
  const ordinal = typeof entry.seq === 'number' ? entry.seq : undefined;
  for (const part of messageParts(entry.data)) {
    applyContentPart(part, sessionId, out, undefined, ordinal);
  }
}

/** Fold one `compaction` row: `data.summary` is the replacement text
 *  and `surface_op` names the span it shadowed. It produces a
 *  timeline marker, not a replace — the reader can still see what
 *  was compacted away and why older turns stopped being sent. */
function applyCompactionRow(entry: SessionTurn, sessionId: string, out: ChatMessageLike[]): void {
  const obj = toRecord(entry.data);
  const summary = obj && typeof obj.summary === 'string' ? obj.summary : '';
  const shadowedTokens =
    obj && typeof obj.shadowed_token_count === 'number' ? obj.shadowed_token_count : undefined;
  const op = replaceOp(entry.surface_op);
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
}

/** Fold one `plugin` row: `kind` is the plugin's own event name and
 *  `payload` its structured body. Rendered as a notice — the row is
 *  provenance, not a model turn. */
function applyPluginRow(entry: SessionTurn, sessionId: string, out: ChatMessageLike[]): void {
  const obj = toRecord(entry.data);
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
}

/** Fold one `subagent_enter` row: the delegation marker the
 *  sub-agent session router writes into the PARENT's log — the child
 *  session id and peer agent. The child's own content lives in its
 *  own session; the parent keeps the link, not the trace. */
function applySubagentEnterRow(out: ChatMessageLike[], entry: SessionTurn): void {
  const obj = toRecord(entry.data);
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
}

/** Fold one `subagent_exit` row: the parent-log marker carrying the
 *  child's terminal status. */
function applySubagentExitRow(out: ChatMessageLike[], entry: SessionTurn): void {
  const obj = toRecord(entry.data);
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
}

/** Fold one `warning` row: a durable fact about the session the
 *  runtime could not report any other way (`data.kind` is a stable
 *  slug — `strict_fold_refused`, `config_drift` — and
 *  `data.message` says what happened). Rendered as a one-line
 *  transcript notice, not a bubble and not a toast: a toast is for
 *  ephemeral things, and the fact outlives the turn. */
function applyWarningRow(entry: SessionTurn, sessionId: string, out: ChatMessageLike[]): void {
  const obj = toRecord(entry.data);
  const message = obj && typeof obj.message === 'string' && obj.message ? obj.message : null;
  const kind = obj && typeof obj.kind === 'string' && obj.kind ? obj.kind : null;
  const content = message ?? (kind ? `会话告警（${kind}）` : '会话告警');
  out.push({
    id: crypto.randomUUID(),
    role: 'assistant',
    segments: [
      {
        id: crypto.randomUUID(),
        type: 'warning',
        content,
      },
    ],
    sessionId,
    status: 'completed',
  });
}

/** Fold one `hook_block` row: a policy refused a tool call. The row
 *  names the policy (`data.hook`), the reason it gave
 *  (`data.reason`) and the call it refused (`data.tool`).
 *
 *  It is its own segment type, not the `warning` one: a warning is a
 *  lifecycle fact about the session, a hook block is a policy that
 *  stopped a tool call — the reader tells them apart, so the page
 *  draws them apart.
 *
 *  The harness writes the row **between** the assistant row that
 *  proposed the call and the `tool_result` row that answers it, so a
 *  fresh message here would fence that pair: `hook_block` carries no
 *  model content, `mutableAssistantTarget` would stop at it, and the
 *  error result would land as a bubble of its own while the call's
 *  tool block stayed pending forever. The notice therefore joins the
 *  assistant bubble it belongs to whenever one is open, and only
 *  stands alone when there is nothing to join. */
function applyHookBlockRow(entry: SessionTurn, sessionId: string, out: ChatMessageLike[]): void {
  const obj = toRecord(entry.data);
  const hook = obj && typeof obj.hook === 'string' && obj.hook ? obj.hook : null;
  const reason = obj && typeof obj.reason === 'string' && obj.reason ? obj.reason : null;
  const tool = obj && typeof obj.tool === 'string' && obj.tool ? obj.tool : null;
  const segment: ChatSegmentLike = {
    id: crypto.randomUUID(),
    type: 'hook_block',
    content: hookBlockNotice(hook, tool, reason),
  };
  const owner = mutableAssistantTarget(out);
  if (owner) {
    owner.segments.push(segment);
    return;
  }
  out.push({
    id: crypto.randomUUID(),
    role: 'assistant',
    segments: [segment],
    sessionId,
    status: 'completed',
  });
}

/** The one line a policy block reads as: the glyph, the policy that
 *  refused, the call it refused and the reason it gave. */
function hookBlockNotice(hook: string | null, tool: string | null, reason: string | null): string {
  const named = hook && tool ? `${hook} 拦截了 ${tool}` : (hook ?? tool);
  const parts = [named, reason].filter((part): part is string => part !== null && part !== '');
  return `⛔ 策略拦截${parts.length > 0 ? ` · ${parts.join(' — ')}` : ''}`;
}

/** What one tag of the typed row vocabulary does when the
 *  projection folds it: a handler renders the row; `null` is
 *  ignored-by-design — log-only bookkeeping the transcript
 *  deliberately does not show. Exhaustive over the generated
 *  `SessionEventType` union: a tag added to the vocabulary fails
 *  `tsc` until this table accounts for it. */
type TypedRowHandler = (entry: SessionTurn, sessionId: string, out: ChatMessageLike[]) => void;

/** The accounting of every tag in the wire vocabulary. */
const TYPED_ROW_TREATMENT: Record<SessionEventType, TypedRowHandler | null> = {
  user_message: (entry, _sessionId, out) => applyUserMessageRow(entry, out),
  assistant_message: applyAssistantMessageRow,
  tool_result: applyToolResultMessage,
  // The durable half of a streamed chunk: the reload's copy of what
  // the live projection showed while the call ran.
  tool_output: (entry, _sessionId, out) => applyToolOutputRow(entry, out),
  compaction: applyCompactionRow,
  warning: applyWarningRow,
  hook_block: applyHookBlockRow,
  plugin: applyPluginRow,
  subagent_enter: (entry, _sessionId, out) => applySubagentEnterRow(out, entry),
  subagent_exit: (entry, _sessionId, out) => applySubagentExitRow(out, entry),
  // ---- ignored by design ----------------------------------
  // Streamed deltas of the running turn; the durable
  // `assistant_message` row renders the finished turn.
  assistant_chunk: null,
  // Compaction lifecycle bookkeeping; the committed `compaction`
  // row is the marker the timeline shows.
  compaction_start: null,
  compaction_summary: null,
  compaction_end: null,
  // A tool call rides its turn's `assistant_message` parts; no
  // separate transcript row.
  tool_call: null,
  // Loop bookkeeping; `ChatPage` reads `step` for scroll hints
  // and `turn` for turn state, outside the message projection.
  step: null,
  turn: null,
  iteration: null,
  // Policy provenance, not transcript content.
  steering_guard: null,
  steering_hint: null,
  // The provider envelope a request ran under.
  request_header: null,
  // Approvals reach the UI through the live `interrupts` window
  // and `ApprovalCard`, not through the transcript.
  approval_asked: null,
  approval_decided: null,
  approval_granted: null,
  // Executor bookkeeping.
  tool_dispatch: null,
  // Resume fingerprint; never transcript content.
  request_snapshot: null,
  // Counters; the context meter carries them.
  usage: null,
  // Run configuration.
  sandbox_mode: null,
  // Writer lifecycle.
  session_shutdown: null,
  // Result-transform provenance.
  tool_result_transformed: null,
  // The consumed `user_message` row renders the turn these book.
  parked_prompt: null,
  park_consumed: null,
  // Log-only by design: never model-visible.
  feedback: null,
};

function applyHistoryEntry(entry: SessionTurn, sessionId: string, out: ChatMessageLike[]): void {
  // The legacy envelope dialect first: those rows pre-date the
  // typed vocabulary, so their `type` strings are read through the
  // widening, not the closed union.
  const envelope = legacyRowType(entry);
  if (envelope === EVENT_USER_INPUT) {
    applyLegacyUserInputRow(entry, out);
    return;
  }
  if (envelope === EVENT_AGENT) {
    applyLegacyAgentRow(entry);
    return;
  }
  if (envelope === EVENT_MODEL) {
    applyLegacyModelRow(entry, sessionId, out);
    return;
  }
  const tag = entry.type;
  if (tag === null || tag === undefined) return;
  // The `?.` is version-skew armour only: the table is total over
  // the union the contract generated, so within one build every
  // tag resolves.
  TYPED_ROW_TREATMENT[tag]?.(entry, sessionId, out);
}

/** Narrow the union `data` shape to a JSON object, or `null`. */
/**
 * The replace range a `surface_op` carries, or null when the op is the
 * append form (`"append"` on the wire).
 */
function replaceOp(
  op: SessionTurn['surface_op'],
): { start: number; end: number; source_event_seqs: number[] } | null {
  return op !== null && op !== undefined && typeof op === 'object' ? op : null;
}

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
 * exception: a `UserInput` or `user_message` row carrying a
 * `surface_op` replace (the row a regenerate persists) first drops the
 * messages its cited seqs shadow, so a rerun shows the replacement
 * story — one prompt bubble, the new answer. The `"append"` string form
 * shadows nothing. A `compaction` row produces a marker instead of a
 * replace, so the reader can still see the span that was compacted.
 */
export function projectRows(state: RowProjection, rows: ReadonlyArray<SessionTurn>): RowProjection {
  const messages = state.messages.slice();
  const sources = state.sources.slice();
  for (const entry of rows) {
    if (legacyRowType(entry) === EVENT_USER_INPUT || entry.type === EVENT_USER_MESSAGE) {
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
  const seqs = replaceOp(entry.surface_op)?.source_event_seqs;
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
        group = { toolUseId, toolName: art.metadata?.tool_name ?? undefined };
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
 * A timeline annotation the transcript renders in-line — a
 * compaction marker, a warning notice, a subagent delegation
 * note. A fact about the session, never an answer: it must not
 * become the target of Regenerate, feedback, or turn-end status
 * painting.
 */
const TIMELINE_ANNOTATION_SEGMENT_TYPES: Record<string, true> = {
  compaction: true,
  warning: true,
  notice: true,
  hook_block: true,
};

/** The shape both helpers need: any message-like row with typed segments. */
interface SegmentbearingMessage {
  role: string;
  segments: ReadonlyArray<{ type: string }>;
}

/** True when a message carries only timeline annotations. */
export function isTimelineAnnotationOnly(message: SegmentbearingMessage): boolean {
  return (
    message.segments.length > 0 &&
    message.segments.every((segment) => TIMELINE_ANNOTATION_SEGMENT_TYPES[segment.type] === true)
  );
}

/**
 * The Regenerate/feedback target: the newest assistant message
 * that is an answer. Timeline annotations are skipped: after a
 * failed turn the last assistant row can be a warning notice, and
 * its fabricated id must never reach `chat/feedback`.
 */
export function selectLastAssistant<T extends SegmentbearingMessage>(
  messages: ReadonlyArray<T>,
): T | null {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message.role !== 'assistant') continue;
    if (isTimelineAnnotationOnly(message)) continue;
    return message;
  }
  return null;
}

/**
 * The `chat/feedback` target for a message: the ordinal of the
 * assistant row it came from, in decimal.
 *
 * The server resolves the target against the session's own rows and
 * refuses one that names no row, so a target that names nothing is
 * not worth sending: `null` when the message carries no ordinal —
 * the live projection, and a bubble no assistant row opened. The
 * rating buttons stay away rather than the server refusing a target
 * that names nothing.
 */
export function feedbackTarget(message: { ordinal?: number } | null | undefined): string | null {
  if (!message) return null;
  if (typeof message.ordinal !== 'number') return null;
  return String(message.ordinal);
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
