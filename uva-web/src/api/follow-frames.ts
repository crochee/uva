/**
 * The frames of the session follow stream, and the validators that
 * decide whether a frame the server sent is one the client may act on.
 *
 * The stream's first frame is a **complete baseline** (header,
 * durable window, cursor, live projection, context meter, open
 * interrupts, request-envelope fingerprint) and every later frame is
 * an increment: a durable `row`, a transient `live` projection, or
 * the changed `interrupts` set.
 *
 * Every validator here takes `unknown` and answers `null` when the
 * frame does not have the shape it expects. That is the point of the
 * module: a frame the server mangled is dropped, never thrown, so a
 * bad frame cannot crash the page. The caller drops what it cannot
 * read — it does not repair it.
 *
 * The validators live apart from `chat-stream.ts` (which opens the
 * stream and applies the frames) so that this defensive parsing can
 * be exercised on its own.
 */

import type { ApprovalRequest, FollowSnapshot, LiveToolCall, LiveTurn, SessionTurn } from './types';

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
      row: SessionTurn;
      /** The request envelope hash, when this row is a snapshot. */
      hash?: string;
    }
  | { type: 'live'; live: LiveTurn }
  | { type: 'interrupts'; pending: ApprovalRequest[] };

/**
 * Validate the fields of a `row` frame the client acts on, so a
 * malformed frame is dropped instead of shifting a cursor or appending
 * a non-row to the window.
 */
export function readRowFrame(data: unknown): FollowEvent | null {
  if (!data || typeof data !== 'object' || !('seq' in data) || !('row' in data)) return null;
  const { seq, row } = data;
  if (typeof seq !== 'number' || !row || typeof row !== 'object' || Array.isArray(row)) return null;
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
    ...(hash ? { hash } : {}),
  };
}

/** Validate the `interrupts` frame's list before trusting its shape. */
export function readInterrupts(data: unknown): ApprovalRequest[] | null {
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
export function readBaseline(data: unknown): FollowSnapshot | null {
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
  // One validator for the interrupt list: the `interrupts` frame and
  // the baseline carry the same entries, so the baseline must not cast
  // them blind. A malformed entry is dropped rather than trusted.
  const pending = readInterrupts(data) ?? [];
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
export function readLiveTurn(data: unknown): LiveTurn | null {
  if (!data || typeof data !== 'object') return null;
  if (!('start_seq' in data) || !('live_seq' in data)) return null;
  if (typeof data.start_seq !== 'number' || typeof data.live_seq !== 'number') return null;
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
    start_seq: data.start_seq,
    live_seq: data.live_seq,
    running,
    text,
    reasoning,
    tool_calls,
  };
}
