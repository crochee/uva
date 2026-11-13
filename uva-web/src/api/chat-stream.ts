/**
 * Chat transport for the uva-web frontend.
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
 * - `POST /api/v1/chat/sessions` — mint the session id. The follow
 *   stream 404s for a session the server does not know, so the page
 *   never guesses an id: creation is the only way a chat starts.
 * - `POST /api/v1/chat/sessions/{id}/messages` — a turn, with
 *   `delivery: "queue" | "steer"`.
 * - `POST /api/v1/sessions/{id}/resume` — the decisions for a
 *   suspended run, with the token rebuilt from the follow baseline.
 * - `POST /api/v1/sessions/{id}/compact` — one manual compaction pass.
 * - `GET /api/v1/sessions/{id}/usage` — session-scoped tokens.
 * - cancel / regenerate / feedback / session detail / model catalog.
 */

import { api } from './client';
import { RpcError, rpc } from './rpc';
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
/** Create a chat session; the server mints the id. */
export async function createChatSession(agentName?: string): Promise<string> {
  const body = agentName === undefined ? {} : { agent_name: agentName };
  const resp = await api.post<{ session_id: string }>('/api/v1/chat/sessions', body);
  return resp.session_id;
}

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
 * issued — or the operator changed a decision the log already holds
 * (the page reconnects, and the new baseline carries a fresh token's
 * inputs), `409 resume_no_snapshot` means this round cannot
 * be resumed at all (the log holds no request envelope, so a new
 * session is the way forward), and a token whose decisions are already
 * in the log **and match them** is not a failure — it is a replay,
 * answered with `200`.
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
  | {
      type: 'row';
      seq: number;
      turnId?: string;
      row: SessionTurn;
      /** The request envelope hash, when this row is a snapshot. */
      hash?: string;
    }
  | { type: 'live'; live: LiveTurn }
  | { type: 'interrupts'; pending: ApprovalRequest[] };

/**
 * A `follow` open that the server refused, carrying the code so the
 * page can tell a transient drop (retry) from a permanent one
 * (`not_found`/`follow_cursor_truncated`: retrying would hammer the
 * same answer forever).
 *
 * `status` keeps the HTTP number the refusal used to travel as —
 * call sites still branch on it — derived from the machine code.
 */
export class FollowHttpError extends Error {
  readonly code: string;
  readonly status: number;

  constructor(code: string, status: number, message: string) {
    super(message);
    this.name = 'FollowHttpError';
    this.code = code;
    this.status = status;
  }
}

/** The HTTP number a refusal code used to travel as. */
const REFUSAL_STATUS: Record<string, number> = {
  not_found: 404,
  follow_cursor_truncated: 410,
};

/** The `session/event` fields the follow loop reads. */
interface SessionEventParams {
  kind?: unknown;
  live?: unknown;
}

/**
 * Turn one notification into a [`FollowEvent`], or `null` when its
 * shape fails validation — a malformed frame is dropped, never
 * yielded, the same contract the SSE reader had.
 */
function frameFromNotification(params: SessionEventParams): FollowEvent | null {
  if (params.kind === 'live' && params.live && typeof params.live === 'object') {
    const live = readLiveTurn(params.live);
    return live ? { type: 'live', live } : null;
  }
  if (params.kind === 'row') {
    return readRowFrame(params);
  }
  if (params.kind === 'interrupts') {
    const pending = readInterrupts(params);
    return pending ? { type: 'interrupts', pending } : null;
  }
  return null;
}

/**
 * Open the session's follow stream and yield its frames.
 *
 * On the RPC wire the baseline is the `sessions/follow` **result**
 * and every later change is a `session/event` notification. When the
 * socket drops, in-flight requests reject; this loop waits for the
 * reconnect and re-follows from the last row it saw — the fresh
 * baseline rebuilds the window (the reconnect timeline rebuild), and
 * the `from` cursor lets the server refuse a reconnect whose window
 * has already scrolled past (`follow_cursor_truncated`).
 *
 * A refusal (`not_found`, truncation) ends the loop by throwing, as
 * the SSE open did; the page decides what to tell the operator.
 */
export async function* followSession(
  sessionId: string,
  options: { signal?: AbortSignal } = {},
): AsyncGenerator<FollowEvent> {
  const signal = options.signal;
  let lastSeq = 0;
  let openedOnce = false;
  while (!signal?.aborted) {
    const queue: FollowEvent[] = [];
    let wake: (() => void) | null = null;
    let reconnected: (() => void) | null = null;
    const unsubscribe = rpc.on('session/event', (raw) => {
      if (!raw || typeof raw !== 'object') return;
      if (Reflect.get(raw, 'session_id') !== sessionId) return;
      const frame = frameFromNotification(raw);
      if (frame) {
        queue.push(frame);
        wake?.();
      }
    });
    const unsubscribeConnection = rpc.onConnection((change) => {
      if (change.state === 'open' && change.reopened) reconnected?.();
    });
    try {
      const baseline = readBaseline(
        await rpc.request<unknown>(
          'sessions/follow',
          {
            session_id: sessionId,
            // The first open asks for the plain window; a reconnect
            // asserts the cursor it left off at.
            ...(openedOnce && lastSeq > 0 ? { from: lastSeq } : {}),
          },
          signal,
        ),
      );
      if (!baseline) {
        throw new Error('the follow baseline failed validation');
      }
      openedOnce = true;
      lastSeq = Math.max(lastSeq, baseline.cursor);
      yield { type: 'baseline', snapshot: baseline };
      // Rows queued while the baseline was in flight may overlap it;
      // the seq check below drops the ones the baseline already
      // carries, so the window is never doubled.
      for (;;) {
        if (signal?.aborted) return;
        const frame = queue.shift();
        if (frame) {
          if (frame.type === 'row') {
            if (frame.seq <= lastSeq) continue;
            lastSeq = frame.seq;
          }
          yield frame;
          continue;
        }
        await new Promise<void>((resolve) => {
          wake = resolve;
        });
        wake = null;
      }
    } catch (error) {
      if (signal?.aborted) return;
      if (error instanceof RpcError) {
        throw new FollowHttpError(error.code, REFUSAL_STATUS[error.code] ?? 500, error.message);
      }
      if (error instanceof DOMException && error.name === 'AbortError') return;
      // The socket dropped: wait for the reconnect, then re-follow
      // from `lastSeq` (a fresh baseline rebuilds the window).
      await new Promise<void>((resolve) => {
        reconnected = resolve;
      });
    } finally {
      unsubscribe();
      unsubscribeConnection();
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
  const hash =
    'request_snapshot_hash' in data &&
    typeof data.request_snapshot_hash === 'string' &&
    data.request_snapshot_hash.length > 0
      ? data.request_snapshot_hash
      : null;
  return {
    type: 'row',
    seq,
    row: row as SessionTurn,
    ...(turnId ? { turnId } : {}),
    ...(hash ? { hash } : {}),
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
