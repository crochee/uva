/**
 * Chat transport for the uva-web frontend.
 *
 * The transcript is one long-lived server stream — `sessions/follow`
 * (opened through `sessionsApi.follow`). Its first frame is a
 * **complete baseline** (header, durable window, cursor, live
 * projection, context meter, open interrupts, request-envelope
 * fingerprint) and every later frame is an increment: a durable
 * `row`, a transient `live` projection, or the changed `interrupts`
 * set. A page load or a reconnect replaces its whole window from that
 * baseline, then applies rows past the cursor — there is no
 * local-cache-plus-stream splice any more.
 *
 * The five `chat/*` actions live here, beside the stream they feed:
 *
 * - `chat/session/create` — mint the session id. The follow stream
 *   answers `not_found` for a session the server does not know, so
 *   the page never guesses an id: creation is the only way a chat
 *   starts.
 * - `chat/message/send` — a turn, with `delivery: "queue" | "steer"`.
 * - `chat/cancel` — drop the in-flight run of a session.
 * - `chat/regenerate` — replay the latest user turn.
 * - `chat/feedback` — a thumbs-up / thumbs-down verdict.
 *
 * Session reads/writes (resume, compact, usage, detail, list,
 * delete) belong to `sessions.ts`; the model catalogue to
 * `catalog.ts`.
 */

import {
  readBaseline,
  readInterrupts,
  readLiveTurn,
  readRowFrame,
  type FollowEvent,
} from './follow-frames';
import { RpcError, rpc } from './rpc';
import { sessionsApi } from './sessions';
import type { SendChatMessageResponse, SteerDelivery } from './types';

/** The `chat/*` methods this module owns. */
const METHOD = {
  createSession: 'chat/session/create',
  send: 'chat/message/send',
  cancel: 'chat/cancel',
  regenerate: 'chat/regenerate',
  feedback: 'chat/feedback',
} as const;

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
  | 'warning'
  | 'hook_block'
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
// Chat actions
// ---------------------------------------------------------------------------

/** Create a chat session; the server mints the id. */
export async function createChatSession(agentName?: string): Promise<string> {
  const body = agentName === undefined ? {} : { agent_name: agentName };
  const resp = await rpc.request<{ session_id: string }>(METHOD.createSession, body);
  return resp.session_id;
}

/**
 * Drop the in-flight run (if any) on a session. Used by the "Stop"
 * affordance. The controller settles every open approval as
 * `cancelled` as part of the same op, so a stop never leaves a
 * session paused forever.
 */
export async function cancelSession(sessionId: string, signal?: AbortSignal): Promise<void> {
  await rpc.request<void>(METHOD.cancel, { session_id: sessionId }, signal);
}

/**
 * Replay the most recent user turn against the same attachments —
 * backs the "Regenerate" button. The rerun's outcome arrives through
 * `follow` like any other turn.
 */
export async function regenerate(sessionId: string, signal?: AbortSignal): Promise<void> {
  await rpc.request<void>(METHOD.regenerate, { session_id: sessionId }, signal);
}

/**
 * Record a thumbs-up / thumbs-down verdict against an assistant
 * message.
 *
 * `targetOrdinal` is the ordinal of the rated row — its `seq`, in
 * decimal. The wire field keeps the name `message_id`: the log
 * format's version gate refuses an old log rather than migrating it,
 * so the field's name is fixed and only its meaning changed.
 */
export async function submitFeedback(
  sessionId: string,
  targetOrdinal: string,
  thumbsUp: boolean,
  signal?: AbortSignal,
): Promise<void> {
  await rpc.request<void>(
    METHOD.feedback,
    // The session rides beside the target: an ordinal only means
    // something inside one session's log, so the server cannot
    // resolve the session from it.
    { message_id: targetOrdinal, thumbs_up: thumbsUp, session_id: sessionId },
    signal,
  );
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
  return rpc.request<SendChatMessageResponse>(
    METHOD.send,
    {
      session_id: sessionId,
      text: options.text,
      ...(attachments.length > 0 ? { attachments } : {}),
      ...(options.agentName ? { agent_name: options.agentName } : {}),
      ...(options.model ? { model: options.model } : {}),
      delivery: options.delivery ?? 'queue',
    },
    options.signal,
  );
}

// ---------------------------------------------------------------------------
// Follow stream
// ---------------------------------------------------------------------------

/**
 * One increment of the session stream.
 *
 * `baseline` is complete on its own; the others are applied on top of
 * the window it established. The frame vocabulary lives beside the
 * validators that read the wire shapes (`follow-frames.ts`); it is
 * re-exported here so this module stays the one import a caller needs.
 */
export type { FollowEvent };

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
        await sessionsApi.follow(
          sessionId,
          // The first open asks for the plain window; a reconnect
          // asserts the cursor it left off at.
          openedOnce && lastSeq > 0 ? { from: lastSeq } : {},
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
