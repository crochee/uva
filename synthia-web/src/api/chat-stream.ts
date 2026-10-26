/**
 * Chat transport for the synthia-web frontend.
 *
 * The transcript is one long-lived server stream —
 * `GET /api/v1/sessions/{id}/follow`. Its first frame is a **complete
 * baseline** (header, durable window, cursor, live projection, context
 * meter, open interrupts, request-envelope fingerprint) and every later
 * frame is an increment: a durable `row`, a transient `live` projection,
 * or the changed `interrupts` set. A page load or a reconnect replaces
 * its whole window from that baseline, then applies rows past the
 * cursor — there is no local-cache-plus-stream splice any more.
 *
 * Everything else is the ordinary v1 REST surface:
 *
 * - `POST /api/v1/chat/sessions/{id}/messages` — a turn, with
 *   `delivery: "queue" | "steer"`.
 * - `POST /api/v1/sessions/{id}/resume` — the decisions for a
 *   suspended run, with the token rebuilt from the follow baseline.
 * - `POST /api/v1/sessions/{id}/compact` — one manual compaction pass.
 * - `GET /api/v1/sessions/{id}/usage` — session-scoped tokens.
 * - cancel / regenerate / feedback / session detail / model catalog.
 */

import { api, resolveApiKey } from './client';
import type {
  ApprovalRequest,
  CompactResult,
  FollowSnapshot,
  LiveToolCall,
  LiveTurn,
  ResumeDecision,
  ResumeToken,
  SendChatMessageResponse,
  SessionDetail,
  SessionTurn,
  SessionUsage,
  SteerDelivery,
} from './types';

// ---------------------------------------------------------------------------
// Segment vocabulary
//
// `MessageSegment` (see `chat-message.ts`) is one row in a rendered
// message. The set is shared with the session-detail projection, so a
// live follow and a rebuilt transcript render the same shapes.
// ---------------------------------------------------------------------------

export type SegmentType =
  | 'text'
  | 'thinking'
  | 'tool_call'
  | 'tool_result'
  | 'tool_block'
  | 'progress'
  | 'notice'
  | 'attachment'
  | 'compaction';

// ---------------------------------------------------------------------------
// Composer attachments
// ---------------------------------------------------------------------------

export type AttachmentPart =
  | { kind: 'text'; text: string }
  | { kind: 'image'; url?: string; dataUrl?: string; mimeType: string; filename?: string }
  | { kind: 'file'; dataUrl: string; mimeType: string; filename?: string };

/**
 * One attachment on the REST wire (`SendMessageRequest.attachments`).
 *
 * The composer holds a `data:` URL; the route wants the bare base64 in
 * `data_base64`, with the MIME type and filename beside it. A remote
 * image becomes a `url` attachment instead, because `image_part` accepts
 * either `data_base64` or `url`.
 */
interface WireAttachment {
  kind: 'image' | 'file' | 'url';
  data_base64?: string;
  url?: string;
  mime_type?: string;
  filename?: string;
}

/** The base64 payload of a `data:` URL, or `undefined` for anything else. */
function dataUrlPayload(dataUrl: string): string | undefined {
  const match = /^data:[^,]*,([\s\S]*)$/s.exec(dataUrl);
  return match ? (match[1] ?? '') : undefined;
}

/**
 * Project the composer's attachments onto the REST chat wire.
 *
 * A part with no bytes and no URL is dropped rather than sent as an
 * empty attachment: the route would skip it anyway, and the composer
 * never produces one (selection validates the MIME type).
 */
function attachmentsToWire(attachments: ReadonlyArray<AttachmentPart>): WireAttachment[] {
  const out: WireAttachment[] = [];
  for (const attachment of attachments) {
    if (attachment.kind === 'text') continue;
    if (attachment.kind === 'image') {
      const payload = attachment.dataUrl ? dataUrlPayload(attachment.dataUrl) : undefined;
      if (payload !== undefined) {
        out.push({
          kind: 'image',
          data_base64: payload,
          mime_type: attachment.mimeType,
          ...(attachment.filename ? { filename: attachment.filename } : {}),
        });
      } else if (attachment.url) {
        out.push({
          kind: 'url',
          url: attachment.url,
          mime_type: attachment.mimeType,
          ...(attachment.filename ? { filename: attachment.filename } : {}),
        });
      }
      continue;
    }
    const payload = dataUrlPayload(attachment.dataUrl);
    if (payload === undefined) continue;
    out.push({
      kind: 'file',
      data_base64: payload,
      mime_type: attachment.mimeType,
      ...(attachment.filename ? { filename: attachment.filename } : {}),
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// REST actions
// ---------------------------------------------------------------------------

/**
 * Drop the in-flight run (if any) on a session. Used by the "Stop"
 * affordance. The controller settles every open approval as
 * `cancelled` as part of the same op, so a stop never leaves a
 * session paused forever.
 */
export async function cancelSession(sessionId: string, signal?: AbortSignal): Promise<void> {
  await api.post<void>(
    `/api/v1/chat/sessions/${encodeURIComponent(sessionId)}/cancel`,
    undefined,
    signal,
  );
}

/**
 * Replay the most recent user turn against the same attachments —
 * backs the "Regenerate" button. The route answers `202`; the rerun's
 * outcome arrives through `follow` like any other turn.
 */
export async function regenerate(sessionId: string, signal?: AbortSignal): Promise<void> {
  await api.post<void>(
    `/api/v1/chat/sessions/${encodeURIComponent(sessionId)}/regenerate`,
    undefined,
    signal,
  );
}

/**
 * Record a thumbs-up / thumbs-down verdict against an assistant
 * message.
 */
export async function submitFeedback(
  sessionId: string,
  messageId: string,
  thumbsUp: boolean,
  signal?: AbortSignal,
): Promise<void> {
  await api.post<void>(
    `/api/v1/chat/messages/${encodeURIComponent(messageId)}/feedback`,
    // The session rides in the body: the message id is a client-side
    // label, so the server cannot resolve the session from it.
    { thumbs_up: thumbsUp, session_id: sessionId },
    signal,
  );
}

/**
 * Fetch one session's durable transcript.
 *
 * Kept for the session-detail view and the regenerate path; the chat
 * page itself takes its window from `follow`.
 */
export async function getSessionDetail(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionDetail> {
  return api.get<SessionDetail>(`/api/v1/sessions/${encodeURIComponent(sessionId)}`, signal);
}

/**
 * Enumerate registered models + the workspace default — backs the
 * model-selector dropdown.
 */
export interface ModelEntry {
  provider: string;
  model: string;
  context_window: number;
  supports_tools: boolean;
  supports_streaming: boolean;
}

export interface ModelsResponse {
  models: ModelEntry[];
  default_provider: string;
  default_model: string;
}

export async function listModels(signal?: AbortSignal): Promise<ModelsResponse> {
  return api.get<ModelsResponse>('/api/v1/models', signal);
}

export interface SendChatMessageOptions {
  text: string;
  attachments?: ReadonlyArray<AttachmentPart>;
  /** `queue` (an ordinary next turn) or `steer` (inject into the live
   *  run at its next step boundary). */
  delivery?: SteerDelivery;
  agentName?: string;
  /** Model this turn runs on, as `"<provider>/<model>"`. */
  model?: string;
  signal?: AbortSignal;
}

/**
 * Submit a user turn.
 *
 * The response says which delivery actually happened: a steer sent to
 * a settled session comes back as `queue`, because the log is the
 * steer's only carrier and no run was live to receive it. A steer is
 * durable before this resolves — the server flushes its row first — so
 * the caller may promise the injection.
 */
export async function sendChatMessage(
  sessionId: string,
  options: SendChatMessageOptions,
): Promise<SendChatMessageResponse> {
  const attachments = attachmentsToWire(options.attachments ?? []);
  return api.post<SendChatMessageResponse>(
    `/api/v1/chat/sessions/${encodeURIComponent(sessionId)}/messages`,
    {
      text: options.text,
      ...(attachments.length > 0 ? { attachments } : {}),
      ...(options.agentName ? { agent_name: options.agentName } : {}),
      ...(options.model ? { model: options.model } : {}),
      delivery: options.delivery ?? 'queue',
    },
    options.signal,
  );
}

/**
 * Resume a suspended run.
 *
 * `decisions` must cover every open interrupt; the server rejects a
 * partial set with `422 incomplete_resume` and does not touch the log.
 *
 * The server tells three outcomes apart, and the page reacts to each:
 * `409 resume_conflict` means the log moved on since the token was
 * issued (the page reconnects, and the new baseline carries a fresh
 * token's inputs), `409 resume_no_snapshot` means this round cannot
 * be resumed at all (the log holds no request envelope, so a new
 * session is the way forward), and a token whose decisions are already
 * in the log is not a failure — it is a replay, answered with `200`.
 */
export async function resumeSession(
  sessionId: string,
  token: ResumeToken,
  decisions: ReadonlyArray<ResumeDecision>,
  options: { allowDrift?: boolean; signal?: AbortSignal } = {},
): Promise<void> {
  await api.post<void>(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/resume`,
    {
      resume_token: token,
      interrupts: decisions,
      ...(options.allowDrift ? { allow_drift: true } : {}),
    },
    options.signal,
  );
}

/**
 * Run one manual compaction pass. The durable rows it writes arrive
 * through `follow`, so the timeline marker needs no return value.
 */
export async function compactSession(
  sessionId: string,
  signal?: AbortSignal,
): Promise<CompactResult> {
  return api.post<CompactResult>(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/compact`,
    undefined,
    signal,
  );
}

/**
 * The fingerprint a `request_snapshot` row stands for.
 *
 * A resume token carries the fingerprint of the request envelope the
 * run was suspended under; the follow baseline reports it, but a run
 * that starts after the baseline stamps a new row with the same
 * meaning. Recomputing it here (same canonical string and hash as
 * `synthia_harness::agent::approval::request_snapshot_hash`) keeps the
 * token from going stale between the page load and the approval
 * click — the fingerprint is a function of the row's own fields, so
 * both sides agree without another round trip.
 *
 * Returns `null` for a row that is not a `request_snapshot` or lacks a
 * field the server hashes.
 */
export async function requestFingerprint(row: SessionTurn): Promise<string | null> {
  if (row.type !== 'request_snapshot') return null;
  // The durable row flattens the envelope onto the row root (see
  // `SessionEvent::RequestSnapshot`); a row that nests it under `data`
  // is read too, so either spelling yields the same fingerprint.
  const nested: unknown = row.data;
  const data: Record<string, unknown> =
    nested && typeof nested === 'object' && !Array.isArray(nested)
      ? (nested as Record<string, unknown>)
      : (row as unknown as Record<string, unknown>);
  const field = (key: string): string | null => (typeof data[key] === 'string' ? data[key] : null);
  const provider = field('provider');
  const model = field('model');
  const params = field('params_hash');
  const tools = field('tools_hash');
  const system = field('system_hash');
  const policy = field('policy_hash');
  if (provider === null || model === null || params === null) return null;
  if (tools === null || system === null || policy === null) return null;
  const sections = Array.isArray(data.sections)
    ? data.sections.filter((section): section is string => typeof section === 'string').join(',')
    : '';
  const canonical = `${provider}|${model}|${params}|${tools}|${system}|${sections}|${policy}`;
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(canonical));
  const hex = Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');
  return `sha256:${hex}`;
}

/** Session-scoped token usage (not the process-wide counters). */
export async function getSessionUsage(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionUsage> {
  return api.get<SessionUsage>(`/api/v1/sessions/${encodeURIComponent(sessionId)}/usage`, signal);
}

// ---------------------------------------------------------------------------
// Follow stream
// ---------------------------------------------------------------------------

/**
 * One increment of the session stream.
 *
 * `baseline` is complete on its own; the others are applied on top of
 * the window it established.
 */
export type FollowEvent =
  | { type: 'baseline'; snapshot: FollowSnapshot }
  | { type: 'row'; seq: number; turnId?: string; row: SessionTurn }
  | { type: 'live'; live: LiveTurn }
  | { type: 'interrupts'; pending: ApprovalRequest[] };

/**
 * A `follow` open that the server refused, carrying the status so the
 * page can tell a transient drop (retry) from a permanent one
 * (`404`/`410`: retrying would hammer the same answer forever).
 */
export class FollowHttpError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = 'FollowHttpError';
    this.status = status;
  }
}

const FOLLOW_URL = (sessionId: string): string =>
  `/api/v1/sessions/${encodeURIComponent(sessionId)}/follow`;

/**
 * The credential the rest of the app authenticates with, in both
 * spellings this server accepts: `x-api-key` is what an Anthropic
 * client sends, `Authorization` is what every other surface here
 * sends. The server resolves `Authorization` first, so the two can
 * never disagree — they are read from one source.
 */
function credentialHeader(): Record<string, string> {
  const key = resolveApiKey();
  if (!key) return {};
  return { 'x-api-key': key, Authorization: `Bearer ${key}` };
}

/**
 * Iterate one SSE stream into named events.
 *
 * Frames are delimited by a blank line (`\n\n`) and carry their name on
 * an `event:` line; `data:` holds one JSON payload. Comment lines (the
 * keep-alive heartbeat) are skipped. A malformed payload is dropped —
 * one bad frame must not tear down a session stream.
 */
async function* parseSseStream(
  reader: ReadableStreamDefaultReader<Uint8Array>,
): AsyncGenerator<{ event: string; data: unknown }> {
  const decoder = new TextDecoder('utf-8');
  let buffer = '';
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let idx = buffer.indexOf('\n\n');
    while (idx !== -1) {
      const raw = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 2);
      let eventName = 'message';
      let dataPayload: unknown = undefined;
      let hasData = false;
      for (const line of raw.split('\n')) {
        if (line.startsWith(':')) continue; // keep-alive comment
        if (line.startsWith('event:')) {
          const trimmed = line.slice(6).trim();
          if (trimmed) eventName = trimmed;
        } else if (line.startsWith('data:')) {
          const payload = line.slice(5).trim();
          if (!payload || payload === '[DONE]') continue;
          try {
            dataPayload = JSON.parse(payload);
            hasData = true;
          } catch {
            // Skip the malformed frame; the next one still flows.
          }
        }
      }
      if (hasData) yield { event: eventName, data: dataPayload };
      idx = buffer.indexOf('\n\n');
    }
  }
}

/**
 * Open the session's follow stream and yield its frames.
 *
 * The stream stays open across turns (a settled turn is not the end of
 * the session), so the caller owns its lifetime: abort the signal to
 * close it. A transport failure is thrown, not yielded — the caller
 * reconnects, and the next baseline replaces whatever it had.
 */
export async function* followSession(
  sessionId: string,
  options: { signal?: AbortSignal } = {},
): AsyncGenerator<FollowEvent> {
  let response: Response;
  try {
    response = await fetch(FOLLOW_URL(sessionId), {
      method: 'GET',
      credentials: 'same-origin',
      headers: credentialHeader(),
      signal: options.signal,
    });
  } catch (err) {
    if (err instanceof Error && err.name === 'AbortError') return;
    throw err;
  }
  if (!response.ok || !response.body) {
    throw new FollowHttpError(response.status, await errorBody(response, 'follow open failed'));
  }
  const reader = response.body.getReader();
  try {
    for await (const { event, data } of parseSseStream(reader)) {
      switch (event) {
        case 'baseline': {
          const snapshot = readBaseline(data);
          if (snapshot) yield { type: 'baseline', snapshot };
          break;
        }
        case 'row': {
          const frame = readRowFrame(data);
          if (frame) yield frame;
          break;
        }
        case 'live': {
          const live = readLiveTurn(data);
          if (live) yield { type: 'live', live };
          break;
        }
        case 'interrupts': {
          const pending = readInterrupts(data);
          if (pending) yield { type: 'interrupts', pending };
          break;
        }
        default:
          // `ping` and any frame a future revision adds: never break
          // the stream over an unknown event.
          break;
      }
    }
  } finally {
    try {
      await reader.cancel();
    } catch {
      // already released
    }
  }
}

/**
 * Validate the fields of a `row` frame the client acts on, so a
 * malformed frame is dropped instead of shifting a cursor or appending
 * a non-row to the window.
 */
function readRowFrame(data: unknown): FollowEvent | null {
  if (!data || typeof data !== 'object' || !('seq' in data) || !('row' in data)) return null;
  const { seq, row } = data;
  if (typeof seq !== 'number' || !row || typeof row !== 'object' || Array.isArray(row)) return null;
  const turnId =
    'turn_id' in data && typeof data.turn_id === 'string' && data.turn_id.length > 0
      ? data.turn_id
      : null;
  return {
    type: 'row',
    seq,
    row: row as SessionTurn,
    ...(turnId ? { turnId } : {}),
  };
}

/** Validate the `interrupts` frame's list before trusting its shape. */
function readInterrupts(data: unknown): ApprovalRequest[] | null {
  if (!data || typeof data !== 'object' || !('pending_interrupts' in data)) return null;
  const pending = data.pending_interrupts;
  if (!Array.isArray(pending)) return null;
  const out: ApprovalRequest[] = [];
  for (const entry of pending) {
    if (!entry || typeof entry !== 'object') continue;
    const strings = ['id', 'call_id', 'tool', 'arguments_hash', 'agent'] as const;
    if (
      strings.some((key) => {
        const value = Reflect.get(entry, key);
        return typeof value !== 'string';
      })
    ) {
      continue;
    }
    const risk = Reflect.get(entry, 'risk');
    if (risk !== 'low' && risk !== 'high' && risk !== 'unknown') continue;
    out.push({
      id: Reflect.get(entry, 'id') as string,
      call_id: Reflect.get(entry, 'call_id') as string,
      tool: Reflect.get(entry, 'tool') as string,
      arguments_hash: Reflect.get(entry, 'arguments_hash') as string,
      risk,
      agent: Reflect.get(entry, 'agent') as string,
    });
  }
  return out;
}

/**
 * Validate a `baseline` frame.
 *
 * The baseline is the one frame the client replaces its whole window
 * with, so the fields it cannot proceed without are checked here; the
 * nested row payloads are the same durable rows the session-detail
 * projection already reads.
 */
function readBaseline(data: unknown): FollowSnapshot | null {
  if (!data || typeof data !== 'object') return null;
  if (
    !('header' in data) ||
    !('rows' in data) ||
    !('cursor' in data) ||
    !('live' in data) ||
    !('context' in data)
  ) {
    return null;
  }
  const { header, rows, cursor, live, context } = data;
  if (!header || typeof header !== 'object') return null;
  if (!Array.isArray(rows) || typeof cursor !== 'number') return null;
  if (!live || typeof live !== 'object') return null;
  if (!context || typeof context !== 'object') return null;
  const pending =
    'pending_interrupts' in data && Array.isArray(data.pending_interrupts)
      ? (data.pending_interrupts as ApprovalRequest[])
      : [];
  const hash =
    'request_snapshot_hash' in data && typeof data.request_snapshot_hash === 'string'
      ? data.request_snapshot_hash
      : '';
  return {
    header: header as FollowSnapshot['header'],
    rows: rows as SessionTurn[],
    cursor,
    live: live as FollowSnapshot['live'],
    context: context as FollowSnapshot['context'],
    pending_interrupts: pending,
    request_snapshot_hash: hash,
  };
}

/**
 * Validate a `live` frame.
 *
 * The projection is rendered wholesale, so its tool-call array is
 * rebuilt from checked fields rather than trusted: a malformed call
 * cannot become a `JSON.stringify` of an arbitrary value or a block
 * keyed by a non-string id.
 */
function readLiveTurn(data: unknown): LiveTurn | null {
  if (!data || typeof data !== 'object') return null;
  if (!('turn_id' in data) || !('live_seq' in data)) return null;
  if (typeof data.turn_id !== 'string' || typeof data.live_seq !== 'number') return null;
  const text = 'text' in data && typeof data.text === 'string' ? data.text : '';
  const reasoning = 'reasoning' in data && typeof data.reasoning === 'string' ? data.reasoning : '';
  const running = 'running' in data ? data.running === true : false;
  const rawCalls: unknown = 'tool_calls' in data ? data.tool_calls : [];
  const tool_calls: LiveToolCall[] = [];
  if (Array.isArray(rawCalls)) {
    for (const raw of rawCalls) {
      if (!raw || typeof raw !== 'object') continue;
      const callId = Reflect.get(raw, 'call_id');
      const name = Reflect.get(raw, 'name');
      const status = Reflect.get(raw, 'status');
      if (typeof callId !== 'string' || typeof name !== 'string') continue;
      if (status !== 'running' && status !== 'completed' && status !== 'failed') continue;
      const output = Reflect.get(raw, 'output');
      tool_calls.push({
        call_id: callId,
        name,
        arguments: Reflect.get(raw, 'arguments') ?? null,
        status,
        ...(typeof output === 'string' ? { output } : {}),
      });
    }
  }
  return {
    turn_id: data.turn_id,
    live_seq: data.live_seq,
    running,
    text,
    reasoning,
    tool_calls,
  };
}

/** Read the server's error envelope for a rejected request. */
async function errorBody(response: Response, fallback: string): Promise<string> {
  const text = await response.text().catch(() => '');
  if (!text) return `${fallback}: HTTP ${response.status}`;
  try {
    const parsed = JSON.parse(text) as {
      code?: string;
      message?: string;
      error?: { message?: string };
    };
    const message = parsed.error?.message ?? parsed.message;
    if (typeof message === 'string' && message.length > 0) {
      return `${message} (HTTP ${response.status})`;
    }
  } catch {
    // Not JSON — fall through to the raw body.
  }
  return `${text} (HTTP ${response.status})`;
}
