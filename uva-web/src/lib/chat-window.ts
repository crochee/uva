/**
 * The chat window's read model.
 *
 * The window is what the chat page renders: the durable rows folded
 * into a transcript, the live projection, the steer chips, the queued
 * messages and the cursor they all hang off. It is a **read model** —
 * it never writes, it only answers "what does the window look like
 * now, given what just arrived".
 *
 * It used to live inside the page component, which meant the page and
 * the model churned in different rhythms: the transcript fold has its
 * own module and its own tests, while the window — twelve actions,
 * the turn and steer semantics, the supersession wiring — was
 * private to the page and reachable only by rendering it.
 *
 * This module is that read model, extracted whole: the reducer, the
 * state and action types, and the row semantics it answers with. The
 * page is React glue over it. Every function here is pure: the
 * timestamps arrive in the actions, so a test can drive the whole
 * window without a browser.
 */

import type { MessageSegment } from '../api/chat-message';
import type {
  ApprovalRequest,
  FollowSnapshot,
  LiveTurn,
  SessionContextMeter,
  SessionTurn,
  SessionUsage,
} from '../api/types';
import type { SteerRecord, SteerState } from '../components/chat/SteerChip';
import { liveFrameAdmitted, retiresProjection } from './live-projection';
import {
  boundaryKind,
  carriesModelContent,
  isTimelineAnnotationOnly,
  messageText,
  newRowProjection,
  projectRows,
  type RowProjection,
} from './session-to-messages';

/** The row's payload read as a keyed object, or null when it is not one. */
function rowData(row: SessionTurn): Record<string, unknown> | null {
  const data = row.data;
  return data !== null && data !== undefined && typeof data === 'object' && !Array.isArray(data)
    ? (data as Record<string, unknown>)
    : null;
}

/**
 * The turn-end reason on a durable `turn` row, or `null` for anything
 * that is not a boundary close.
 */
export function turnEndReason(row: SessionTurn): string | null {
  if (row.type !== 'turn') return null;
  const data = rowData(row);
  if (boundaryKind(data) !== 'end') return null;
  return data && typeof data.reason === 'string' ? data.reason : 'completed';
}

/**
 * The step ordinal a `step` start row opens, or `null` for anything
 * else. A steer is "injected at step N" exactly when such a row lands
 * after the steer's own row.
 */
function stepStartIndex(row: SessionTurn): number | null {
  if (row.type !== 'step') return null;
  const data = rowData(row);
  if (boundaryKind(data) !== 'start') return null;
  return data && typeof data.step === 'number' ? data.step : -1;
}

/** A row whose payload the contract types as the provider `Message`. */
type MessageRow = Extract<SessionTurn, { type?: 'user_message' | 'assistant_message' }>;

/**
 * The user text of a prompt row.
 *
 * A queued turn is logged as the provider message the run sends
 * (`user_message`); a steer is the same row with
 * `source: "steer"`. The text is the reconciliation key
 * for the composer's local outbox and for matching a steer to its row.
 */
function promptTextOf(row: MessageRow): string {
  return row.data ? messageText(row.data) : '';
}

/** Translate a turn-end reason onto the status pill's vocabulary. */
/**
 * The message a failed turn carries.
 *
 * The turn-end row names the outcome (`reason`) and, for a failure,
 * the message the run stopped with. A run that died before it said
 * anything has no assistant bubble to carry the failure, so the page
 * shows it here instead of returning to "idle" as if nothing
 * happened.
 */
function turnErrorMessage(row: SessionTurn): string | null {
  const reason = turnEndReason(row);
  if (reason === null || statusFromReason(reason) !== 'failed') return null;
  const data = row.data;
  if (data && typeof data === 'object' && !Array.isArray(data)) {
    const message = (data as Record<string, unknown>).message;
    if (typeof message === 'string' && message.trim() !== '') {
      return message;
    }
  }
  return 'The run failed before it produced an answer.';
}

function statusFromReason(reason: string | null): string {
  if (reason === null) return 'completed';
  const normalized = reason.toLowerCase();
  if (/cancel/.test(normalized)) return 'canceled';
  if (/suspend|approval/.test(normalized)) return 'input-required';
  if (/fail|error|drift/.test(normalized)) return 'failed';
  return 'completed';
}

/**
 * Move every steer's state forward for one durable row.
 *
 * The log is the only truth here — there is no steer queue in memory to
 * consult. In order: the steer's own row makes it durable (`accepted` →
 * positioned), a later step start injects it at that step, an assistant
 * row in that step consumes it, and a turn close turns a steer that
 * never reached a boundary into a queued turn (`next turn`).
 */
function advanceSteers(
  steers: ReadonlyArray<SteerRecord>,
  row: SessionTurn,
  seq: number,
): ReadonlyArray<SteerRecord> {
  if (row.type === 'user_message' && row.source === 'steer') {
    const text = promptTextOf(row);
    let claimed = false;
    return steers.map((steer) => {
      if (
        claimed ||
        steer.rowSeq !== undefined ||
        steer.state !== 'accepted' ||
        steer.text !== text
      ) {
        return steer;
      }
      claimed = true;
      return { ...steer, rowSeq: seq };
    });
  }
  const step = stepStartIndex(row);
  if (step !== null) {
    return steers.map((steer) =>
      steer.rowSeq !== undefined && steer.rowSeq < seq && steer.state === 'accepted'
        ? { ...steer, state: 'injected', step }
        : steer,
    );
  }
  if (row.type === 'assistant_message') {
    return steers.map((steer) =>
      steer.state === 'injected' ? { ...steer, state: 'consumed' } : steer,
    );
  }
  if (turnEndReason(row) !== null) {
    return steers.map((steer) =>
      steer.state === 'accepted' ? { ...steer, state: 'next_turn' } : steer,
    );
  }
  return steers;
}

/** A message the user queued that the log has not logged yet. */
export interface QueuedMessage {
  id: string;
  text: string;
}

/**
 * Drop the queued message a newly logged prompt answered. Matching by
 * text is enough: the composer only ever has one outstanding queue item
 * per distinct text, and the row is the server's confirmation that it
 * arrived.
 */
function settleQueued(queued: QueuedMessage[], row: SessionTurn): QueuedMessage[] {
  if (row.type !== 'user_message') return queued;
  const text = promptTextOf(row);
  const index = queued.findIndex((item) => item.text === text);
  if (index === -1) return queued;
  return [...queued.slice(0, index), ...queued.slice(index + 1)];
}

/** Everything the `follow` stream has taught the page. */
export interface ChatWindow {
  sessionId: string;
  /** Last durable ordinal applied; rows at or below it are duplicates. */
  cursor: number;
  projection: RowProjection;
  /** The in-progress turn's transient projection, when it is usable. */
  live: LiveTurn | null;
  /** Highest `live_seq` applied (a frame at or below it predates us). */
  liveSeq: number;
  /**
   * A turn whose durable row superseded its live projection. Later
   * `live` frames for that turn are ignored — the projection is
   * cumulative, so re-showing it would duplicate what the row carries.
   */
  supersededStart: number | null;
  /** First-seen timestamp per live tool call, for the timeout label.
   *  Stamped when the frame is applied (not during render). */
  liveCallStartedAt: Record<string, number>;
  context: SessionContextMeter | null;
  interrupts: ApprovalRequest[];
  requestHash: string;
  usage: SessionUsage | null;
  /** The last turn's failure message, when the run ended in error. */
  turnError: string | null;
  steers: ReadonlyArray<SteerRecord>;
  queued: QueuedMessage[];
  /** Whether the reader asked for the full transcript (beyond the
   *  baseline's fixed window). */
  historyExpanded: boolean;
}

/** The current turn's already-durable projection. */
export interface TurnDurable {
  text: string;
  reasoning: string;
  callIds: string[];
}

/** True for the `turn { start }` row that opens a run's turn. */
function isTurnStart(row: SessionTurn): boolean {
  if (row.type !== 'turn') return false;
  return boundaryKind(rowData(row)) === 'start';
}

/**
 * The live projection's already-durable prefix for the current turn.
 *
 * Derived from the **projection the page renders**, never from a
 * separate accumulator: the durable prefix is exactly the assistant
 * content the window shows for the live run, so the live item renders
 * `live.text` minus it — the only point at which durable and transient
 * content can meet without duplicating.
 *
 * The live projection accumulates the whole run, so the prefix is
 * *every* content-bearing assistant message since the last user row, in
 * order — not just the trailing one:
 *
 * - a `task` call's `subagent_enter` / `subagent_exit` bubbles are
 *   `role: 'subagent'` and sit after the parent assistant message, so a
 *   trailing-message-only scan would report an empty prefix and the live
 *   item would re-render durable text and tool blocks;
 * - a compaction marker is assistant-shaped but carries only a
 *   `compaction` segment. It is a divider, not a durable prefix, and the
 *   fold starts a new assistant bubble after it. Stopping at the marker
 *   would blank the live remainder once `live.text` spans the run.
 *
 * Messages are concatenated in chronological order, matching the way
 * `projectRows` coalesces ordinary assistant rows into one message, so
 * `live.text` starts with the result. A leading user row opens the turn;
 * before it lands there is no durable content for the live run yet.
 */
export function projectedDurable(projection: RowProjection): TurnDurable {
  let start = 0;
  for (let i = projection.messages.length - 1; i >= 0; i -= 1) {
    if (projection.messages[i].role === 'user') {
      start = i + 1;
      break;
    }
  }
  const text: string[] = [];
  const reasoning: string[] = [];
  const callIds: string[] = [];
  for (let i = start; i < projection.messages.length; i += 1) {
    const message = projection.messages[i];
    if (!carriesModelContent(message)) continue;
    for (const segment of message.segments) {
      if (segment.type === 'text') {
        text.push(segment.content);
        continue;
      }
      if (segment.type === 'thinking') {
        reasoning.push(segment.content);
        continue;
      }
      if (segment.type === 'tool_block' && segment.toolUseId) callIds.push(segment.toolUseId);
    }
  }
  return { text: text.join(''), reasoning: reasoning.join(''), callIds };
}

/**
 * Rebuild the steer chips from a baseline window.
 *
 * A steer's states are a property of the log (its own row, the step
 * boundary that carried it, the assistant row that consumed it), so a
 * reload can show where every steer in the current turn went instead of
 * starting empty. Chips from earlier turns are not reconstructed: their
 * story is over and the chips would be permanent noise.
 */
function steersFromRows(rows: ReadonlyArray<SessionTurn>): SteerRecord[] {
  let start = -1;
  for (let i = rows.length - 1; i >= 0; i -= 1) {
    if (isTurnStart(rows[i])) {
      start = i;
      break;
    }
  }
  let steers: ReadonlyArray<SteerRecord> = [];
  for (let i = start + 1; i < rows.length; i += 1) {
    const row = rows[i];
    const seq = row.seq ?? 0;
    if (row.type === 'user_message' && row.source === 'steer') {
      steers = [
        ...steers,
        { id: crypto.randomUUID(), text: promptTextOf(row), state: 'accepted', rowSeq: seq },
      ];
      continue;
    }
    steers = advanceSteers(steers, row, seq);
  }
  return [...steers];
}

/**
 * Fold a reconnected baseline's steer rows over the chips this page
 * still holds.
 *
 * The rebuilt set is server truth (its rows carry the seq and the
 * boundaries that moved each chip's state). A chip still `accepted`
 * with no row yet — a request in flight when the connection dropped —
 * has nothing to reconcile against, so it is kept instead of vanishing;
 * once its row arrives the normal row path positions it. A chip whose
 * text is already in the rebuilt set yields to the row-derived one.
 */
function reconcileSteers(
  rebuilt: ReadonlyArray<SteerRecord>,
  local: ReadonlyArray<SteerRecord>,
): ReadonlyArray<SteerRecord> {
  const texts = new Set(rebuilt.map((steer) => steer.text));
  const inFlight = local.filter(
    (steer) => steer.state === 'accepted' && steer.rowSeq === undefined && !texts.has(steer.text),
  );
  return [...rebuilt, ...inFlight];
}

export type WindowAction =
  | { kind: 'reset'; sessionId: string }
  | { kind: 'baseline'; sessionId: string; snapshot: FollowSnapshot }
  | { kind: 'row'; seq: number; row: SessionTurn }
  | { kind: 'live'; live: LiveTurn; now: number }
  | { kind: 'interrupts'; pending: ApprovalRequest[] }
  | { kind: 'request-hash'; hash: string }
  | { kind: 'usage'; usage: SessionUsage }
  | { kind: 'history'; projection: RowProjection }
  | { kind: 'queued'; id: string; text: string }
  | { kind: 'steer'; id: string; text: string; state: SteerState }
  | { kind: 'steer-fallback'; id: string }
  | { kind: 'steers-returned'; ids: ReadonlyArray<string> };

export function emptyWindow(sessionId: string): ChatWindow {
  return {
    sessionId,
    cursor: 0,
    projection: newRowProjection(sessionId),
    live: null,
    liveSeq: 0,
    supersededStart: null,
    liveCallStartedAt: {},
    context: null,
    interrupts: [],
    requestHash: '',
    usage: null,
    turnError: null,
    steers: [],
    queued: [],
    historyExpanded: false,
  };
}

/**
 * Fold one follow increment into the page's window.
 *
 * The baseline is the only place the window is *replaced*; every other
 * action applies on top of it. Rows past the cursor append to the
 * projection (never rebuild it, so ids stay stable and the list does not
 * remount); a durable row for the live turn retires that turn's
 * projection, and a live frame that is not newer than the baseline's is
 * discarded.
 */
export function windowReducer(state: ChatWindow, action: WindowAction): ChatWindow {
  switch (action.kind) {
    case 'reset':
      return emptyWindow(action.sessionId);
    case 'baseline': {
      const { snapshot } = action;
      const projection = snapshot.live.live_seq > 0 ? snapshot.live : null;
      // The baseline's rows may already carry the projection's own
      // content — or the turn boundary that closed its run. The
      // module owns the retirement question, so the baseline asks the
      // same question an incremental row would.
      const retired =
        projection !== null &&
        snapshot.rows.some((row) =>
          retiresProjection(row, row.seq ?? 0, {
            startSeq: projection.start_seq,
            running: projection.running,
          }),
        );
      return {
        ...state,
        sessionId: action.sessionId,
        cursor: snapshot.cursor,
        projection: projectRows(newRowProjection(action.sessionId), snapshot.rows),
        live: retired ? null : projection,
        liveSeq: snapshot.live.live_seq,
        supersededStart: retired && projection !== null ? projection.start_seq : null,
        liveCallStartedAt: {},
        context: snapshot.context,
        interrupts: snapshot.pending_interrupts,
        requestHash: snapshot.request_snapshot_hash,
        // The log is the only source for a steer's story, so a fresh
        // window reconstructs the current turn's chips from it; a steer
        // still in flight (no row yet) survives the reconnect.
        steers: reconcileSteers(steersFromRows(snapshot.rows), state.steers),
        historyExpanded: false,
      };
    }
    case 'row': {
      // A reconnect's baseline already delivered this row, and the
      // stream's `?from=` contract is "apply rows past the cursor".
      if (action.seq <= state.cursor) return state;
      let projection = projectRows(state.projection, [action.row]);
      const reason = turnEndReason(action.row);
      if (reason !== null) {
        const status = statusFromReason(reason);
        const messages = projection.messages.slice();
        for (let i = messages.length - 1; i >= 0; i -= 1) {
          const message = messages[i];
          if (message.role !== 'assistant') continue;
          // A timeline annotation (a trailing warning notice after a
          // failed turn) is not the turn's answer: the failure status
          // belongs on the newest real answer, if one exists.
          if (isTimelineAnnotationOnly(message)) continue;
          messages[i] = { ...message, status };
          break;
        }
        projection = { ...projection, messages };
      }
      const starting = isTurnStart(action.row);
      // A failed turn is a fact the page has to show: the run may
      // have died before any assistant bubble existed.
      const failed = turnErrorMessage(action.row);
      const turnError = failed ?? (starting ? null : state.turnError);
      // Retire the live projection when the durable row now represents
      // it. The rule lives in the module: a content row whose ordinal
      // is past the projection's start position is the step's own
      // answer, and a turn boundary for a run that is already over
      // leaves nothing to replace it.
      let live = state.live;
      let supersededStart = state.supersededStart;
      const current = live;
      if (current) {
        const retired = retiresProjection(action.row, action.seq, {
          startSeq: current.start_seq,
          running: current.running,
        });
        if (retired) {
          supersededStart = current.start_seq;
          live = null;
        }
      }
      let steers = advanceSteers(state.steers, action.row, action.seq);
      // A finished or freshly opened turn retires chips whose story is
      // over; only an accepted/injected steer still needs watching.
      if (starting || reason !== null) {
        steers = steers.filter((steer) => steer.state === 'accepted' || steer.state === 'injected');
      }
      return {
        ...state,
        cursor: action.seq,
        projection,
        turnError,
        steers,
        queued: settleQueued(state.queued, action.row),
        live,
        supersededStart,
      };
    }
    case 'live': {
      const { live } = action;
      // A durable row for this step already told the truth; the
      // projection would duplicate it.
      if (!liveFrameAdmitted(live.start_seq, state.supersededStart)) return state;
      // A frame older than the last accepted one predates the window the
      // client accepted.
      if (live.live_seq < state.liveSeq) return state;
      const startedAt: Record<string, number> = { ...state.liveCallStartedAt };
      for (const call of live.tool_calls) {
        startedAt[call.call_id] ??= action.now;
      }
      // Keep the map bounded: only the calls this projection still
      // shows need a start time.
      const active = new Set(live.tool_calls.map((call) => call.call_id));
      for (const key of Object.keys(startedAt)) {
        if (!active.has(key)) delete startedAt[key];
      }
      return {
        ...state,
        live,
        liveSeq: live.live_seq,
        liveCallStartedAt: startedAt,
        // A later step's position means a newer projection; the mark of an
        // older retired one must not silence it.
        supersededStart: state.supersededStart,
      };
    }
    case 'interrupts':
      return { ...state, interrupts: action.pending };
    case 'request-hash':
      return { ...state, requestHash: action.hash };
    case 'usage':
      return { ...state, usage: action.usage };
    case 'history':
      return { ...state, projection: action.projection, historyExpanded: true };
    case 'queued':
      return { ...state, queued: [...state.queued, { id: action.id, text: action.text }] };
    case 'steer':
      return {
        ...state,
        steers: [...state.steers, { id: action.id, text: action.text, state: action.state }],
      };
    case 'steer-fallback':
      return {
        ...state,
        steers: state.steers.map((steer) =>
          steer.id === action.id ? { ...steer, state: 'next_turn' } : steer,
        ),
      };
    case 'steers-returned': {
      const removed = new Set(action.ids);
      return { ...state, steers: state.steers.filter((steer) => !removed.has(steer.id)) };
    }
  }
}

/**
 * Build the live turn's segments.
 *
 * The projection is cumulative for the turn, so it is rendered as one
 * message that is *replaced* frame by frame, not appended to. Tool
 * blocks reuse the durable renderer's shape (`tool_block` keyed by the
 * provider's call id), so a call looks the same before and after its
 * result is logged.
 */
export function liveSegments(
  live: LiveTurn,
  durable: TurnDurable,
  callStartedAt: Readonly<Record<string, number>>,
): MessageSegment[] {
  // The projection accumulates the whole run; the durable prefix is
  // already rendered from the log. Showing only the remainder is what
  // keeps a mid-run reconnect (or a resumed step) from duplicating
  // text — and a projection the log does not prefix is dropped rather
  // than trusted twice.
  const text = durable.text
    ? live.text.startsWith(durable.text)
      ? live.text.slice(durable.text.length)
      : ''
    : live.text;
  const reasoning = durable.reasoning
    ? live.reasoning.startsWith(durable.reasoning)
      ? live.reasoning.slice(durable.reasoning.length)
      : ''
    : live.reasoning;
  const segments: MessageSegment[] = [];
  if (reasoning) {
    segments.push({
      id: `live-thinking-${live.start_seq}`,
      type: 'thinking',
      content: reasoning,
    });
  }
  if (text) {
    segments.push({ id: `live-text-${live.start_seq}`, type: 'text', content: text });
  }
  for (const call of live.tool_calls) {
    if (durable.callIds.includes(call.call_id)) continue;
    const startedAt = callStartedAt[call.call_id] ?? Date.now();
    let callBody: string;
    try {
      callBody = JSON.stringify(call.arguments, null, 2) ?? '';
    } catch {
      callBody = String(call.arguments);
    }
    segments.push({
      id: `live-call-${live.start_seq}-${call.call_id}`,
      type: 'tool_block',
      content: '',
      toolName: call.name,
      callContent: callBody,
      ...(typeof call.output === 'string' ? { resultContent: call.output } : {}),
      toolPending: call.status === 'running',
      toolError: call.status === 'failed',
      toolUseId: call.call_id,
      pendingSince: startedAt,
    });
  }
  return segments;
}

/**
 * Repair instructions the server can answer for a failed resume or
 * compaction. The code is the actionable part; the message says what
 * to do next.
 */
