/**
 * Sessions domain client — the durable session surface.
 *
 * The seven methods the frontend uses are registered by
 * `crates/uva-app-server/src/rpc/methods/sessions.rs`:
 * `sessions/list` · `sessions/get` · `sessions/delete` ·
 * `sessions/usage` · `sessions/compact` · `sessions/resume` ·
 * `sessions/follow`.
 *
 * The other session methods the server registers (`status`, `export`,
 * `fork`, `events`, `search`) have no frontend call site and are not
 * mirrored here — a wire addition lands in this file only when a page
 * actually reads it.
 */
import { rpc } from './rpc';
import type {
  CompactResult,
  List,
  ResumeDecision,
  ResumeToken,
  SessionDetail,
  SessionSummary,
  SessionUsage,
} from './types';

const METHOD = {
  list: 'sessions/list',
  get: 'sessions/get',
  remove: 'sessions/delete',
  usage: 'sessions/usage',
  compact: 'sessions/compact',
  resume: 'sessions/resume',
  follow: 'sessions/follow',
} as const;

export interface SessionListParams {
  /** Page size hint. The server clamps to [1, 100]. */
  limit?: number;
  /** Opaque cursor from a previous page's `next_cursor`. */
  cursor?: string;
  /** Sort field; prefix with `-` for descending. */
  sort?: string;
}

export interface ResumeOptions {
  /** Accept a request whose fingerprint drifted since the token was
   *  issued. */
  allowDrift?: boolean;
  signal?: AbortSignal;
}

export interface FollowOptions {
  /** The row sequence a reconnect left off at. The server refuses a
   *  window that already scrolled past (`follow_cursor_truncated`). */
  from?: number;
}

export const sessionsApi = {
  /** `sessions/list` — one page of sessions, newest first by default. */
  list: (params: SessionListParams = {}, signal?: AbortSignal): Promise<List<SessionSummary>> =>
    rpc.request<List<SessionSummary>>(
      METHOD.list,
      {
        ...(params.limit !== undefined ? { limit: params.limit } : {}),
        ...(params.cursor ? { cursor: params.cursor } : {}),
        ...(params.sort ? { sort: params.sort } : {}),
      },
      signal,
    ),

  /** `sessions/get` — one session's durable transcript. */
  get: (id: string, signal?: AbortSignal): Promise<SessionDetail> =>
    rpc.request<SessionDetail>(METHOD.get, { id }, signal),

  /** `sessions/delete` — drop a session and its log. */
  remove: (id: string, signal?: AbortSignal): Promise<void> =>
    rpc.request<void>(METHOD.remove, { id }, signal),

  /** `sessions/usage` — session-scoped token usage (not the
   *  process-wide counters). */
  usage: (sessionId: string, signal?: AbortSignal): Promise<SessionUsage> =>
    rpc.request<SessionUsage>(METHOD.usage, { session_id: sessionId }, signal),

  /** `sessions/compact` — one manual compaction pass. */
  compact: (sessionId: string, signal?: AbortSignal): Promise<CompactResult> =>
    rpc.request<CompactResult>(METHOD.compact, { session_id: sessionId }, signal),

  /**
   * `sessions/resume` — submit the decisions for a suspended run.
   *
   * `decisions` must cover every open interrupt; the server rejects a
   * partial set with `incomplete_resume` and does not touch the log.
   * `resume_conflict` means the log moved on since the token was
   * issued — or an already-logged decision changed — and
   * `resume_no_snapshot` means this round cannot be resumed at all.
   * A token whose decisions are already in the log **and match them**
   * is not a failure: it is a replay, answered as success.
   */
  resume: (
    sessionId: string,
    token: ResumeToken,
    decisions: ReadonlyArray<ResumeDecision>,
    options: ResumeOptions = {},
  ): Promise<void> =>
    rpc.request<void>(
      METHOD.resume,
      {
        session_id: sessionId,
        resume_token: token,
        interrupts: decisions,
        ...(options.allowDrift ? { allow_drift: true } : {}),
      },
      options.signal,
    ),

  /**
   * `sessions/follow` — open the session stream and return its raw
   * baseline. The frame vocabulary and its validators live in
   * `chat-stream.ts` / `follow-frames.ts`; this client owns only the
   * wire name.
   */
  follow: (
    sessionId: string,
    options: FollowOptions = {},
    signal?: AbortSignal,
  ): Promise<unknown> =>
    rpc.request<unknown>(
      METHOD.follow,
      {
        session_id: sessionId,
        ...(options.from !== undefined ? { from: options.from } : {}),
      },
      signal,
    ),
};
