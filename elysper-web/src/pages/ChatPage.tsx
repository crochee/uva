import {
  useState,
  useRef,
  useEffect,
  useLayoutEffect,
  useCallback,
  useMemo,
  useReducer,
  type FormEvent,
  type KeyboardEvent,
  type ChangeEvent,
} from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import { Button } from '../components/ui/Button';
import { Card } from '../components/ui/Card';
import { ChatMessageList, type ChatMessageViewItem } from '../components/chat/ChatMessageView';
import { ApprovalCard, type ApprovalCardProps } from '../components/chat/ApprovalCard';
import { SteerChips, type SteerRecord, type SteerState } from '../components/chat/SteerChip';
import { SessionSidePanel } from '../components/chat/SessionSidePanel';
import { TodoProgress } from '../components/chat/TodoProgress';
import { PromptPrefix } from '../components/chat/PromptPrefix';
import { deriveArtifacts, deriveSubagents, deriveTodos } from '../lib/session-panel-data';
import {
  cancelSession as apiCancelSession,
  compactSession as apiCompactSession,
  FollowHttpError,
  createChatSession,
  followSession,
  getSessionDetail,
  getSessionUsage,
  listModels as apiListModels,
  regenerate as apiRegenerate,
  resumeSession as apiResumeSession,
  sendChatMessage,
  submitFeedback as apiSubmitFeedback,
  type AttachmentPart,
  type ModelEntry,
} from '../api/chat-stream';
import type { MessageSegment } from '../api/chat-message';
import type {
  AgentDetail,
  ApprovalRequest,
  FollowSnapshot,
  List,
  LiveTurn,
  ResumeDecision,
  ResumeStatus,
  SessionContextMeter,
  SessionTurn,
  SessionUsage,
} from '../api/types';
import {
  carriesModelContent,
  messageParts,
  newRowProjection,
  projectRows,
  type RowProjection,
} from '../lib/session-to-messages';
import { useServerHealth, setServerHealth } from '../hooks/useServerHealth';
import { useToast } from '../hooks/useToast';
import { shortId } from '../lib/short-id';
import { ApiError, api } from '../api/client';
import './ChatPage.css';

/**
 * MIME prefixes the chat composer accepts as multimodal
 * attachments. Pictures ride the REST chat wire's `image` part and a
 * document (PDF, plain text, markdown) its `file` part; anything else
 * is refused at selection time.
 */
const ATTACHMENT_IMAGE_PREFIXES = ['image/'];
const ATTACHMENT_FILE_PREFIXES = ['application/pdf', 'text/plain', 'text/markdown'];

/**
 * Slack (px) for "the thread is at the bottom". Fractional scroll
 * offsets and sub-pixel line heights mean an exact comparison is never
 * reliably true, and the container grows between the scroll event and
 * the check.
 */
const NEAR_BOTTOM_PX = 24;

/** How long a dropped `follow` connection waits before reopening. */
const RECONNECT_DELAY_MS = 2000;

/**
 * Classify a `File`'s MIME type into the matching `AttachmentPart`
 * kind. Returns `null` when the MIME type isn't supported — the caller
 * surfaces a toast and skips the file.
 */
function classifyAttachment(mimeType: string): 'image' | 'file' | null {
  if (ATTACHMENT_IMAGE_PREFIXES.some((p) => mimeType.startsWith(p))) return 'image';
  if (ATTACHMENT_FILE_PREFIXES.some((p) => mimeType.startsWith(p))) return 'file';
  return null;
}

/**
 * Read a `File` into a base64 `data:` URL.
 *
 * `FileReader` is callback-based and this project's TypeScript lib is
 * ES2020, so the executor form is the one API that fits (there is no
 * `Promise.withResolvers` in the lib).
 */
function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error('FileReader failed'));
    reader.onload = () => {
      if (typeof reader.result === 'string') resolve(reader.result);
      else reject(new Error('FileReader did not return a string'));
    };
    reader.readAsDataURL(file);
  });
}

/**
 * The turn-end reason on a durable `turn` row, or `null` for anything
 * that is not a boundary close. The log spells the discriminator as
 * `kind` on older writers and `phase` on newer ones, so both are read.
 */
function turnEndReason(row: SessionTurn): string | null {
  if (row.type !== 'turn') return null;
  const data = row.data;
  if (!data || typeof data !== 'object' || Array.isArray(data)) return null;
  const phase =
    typeof data.phase === 'string' ? data.phase : typeof data.kind === 'string' ? data.kind : '';
  if (phase !== 'end') return null;
  return typeof data.reason === 'string' ? data.reason : 'completed';
}

/**
 * The step ordinal a `step` start row opens, or `null` for anything
 * else. A steer is "injected at step N" exactly when such a row lands
 * after the steer's own row.
 */
function stepStartIndex(row: SessionTurn): number | null {
  if (row.type !== 'step') return null;
  const data = row.data;
  if (!data || typeof data !== 'object' || Array.isArray(data)) return null;
  const phase =
    typeof data.phase === 'string' ? data.phase : typeof data.kind === 'string' ? data.kind : '';
  if (phase !== 'start') return null;
  return typeof data.step === 'number' ? data.step : -1;
}

/**
 * The user text of a prompt row.
 *
 * A queued turn is logged as the provider message the run sends
 * (`user_message`, or the legacy `UserInput` envelope); a steer is the
 * same row with `source: "steer"`. The text is the reconciliation key
 * for the composer's local outbox and for matching a steer to its row.
 */
function promptTextOf(row: SessionTurn): string {
  const data = row.data;
  if (
    row.type === 'UserInput' &&
    data &&
    typeof data === 'object' &&
    !Array.isArray(data) &&
    typeof data.text === 'string'
  ) {
    return data.text;
  }
  return messageParts(data)
    .filter((part) => part.type === 'text' && typeof part.text === 'string')
    .map((part) => (typeof part.text === 'string' ? part.text : ''))
    .join('');
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
interface QueuedMessage {
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
  if (row.type !== 'user_message' && row.type !== 'UserInput') return queued;
  const text = promptTextOf(row);
  const index = queued.findIndex((item) => item.text === text);
  if (index === -1) return queued;
  return [...queued.slice(0, index), ...queued.slice(index + 1)];
}

/** Everything the `follow` stream has taught the page. */
interface ChatWindow {
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
  supersededTurn: string | null;
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
interface TurnDurable {
  text: string;
  reasoning: string;
  callIds: string[];
}

/**
 * What one durable `assistant_message` row contributed to the live
 * projection. Used to decide whether an unlabelled row is already
 * represented by the transient view (see the retirement fallback in
 * `windowReducer`). A part list arrives either as provider
 * `ContentPart`s or as the `Single`/`Multi` envelope, which
 * `messageParts` already normalises.
 */
function durableOfMessage(row: SessionTurn): TurnDurable {
  const text: string[] = [];
  const reasoning: string[] = [];
  const callIds: string[] = [];
  for (const part of messageParts(row.data)) {
    if (part.type === 'text' && typeof part.text === 'string') {
      text.push(part.text);
      continue;
    }
    if (part.type === 'reasoning' && typeof part.text === 'string') {
      reasoning.push(part.text);
      continue;
    }
    if (part.type === 'tool_use' && typeof part.id === 'string') callIds.push(part.id);
  }
  return { text: text.join(''), reasoning: reasoning.join(''), callIds };
}

/** True for the `turn { start }` row that opens a run's turn. */
function isTurnStart(row: SessionTurn): boolean {
  if (row.type !== 'turn') return false;
  const data = row.data;
  if (!data || typeof data !== 'object' || Array.isArray(data)) return false;
  const phase =
    typeof data.phase === 'string' ? data.phase : typeof data.kind === 'string' ? data.kind : '';
  return phase === 'start';
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
function projectedDurable(projection: RowProjection): TurnDurable {
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

type WindowAction =
  | { kind: 'reset'; sessionId: string }
  | { kind: 'baseline'; sessionId: string; snapshot: FollowSnapshot }
  | { kind: 'row'; seq: number; row: SessionTurn; turnId?: string }
  | { kind: 'live'; live: LiveTurn; now: number }
  | { kind: 'interrupts'; pending: ApprovalRequest[] }
  | { kind: 'request-hash'; hash: string }
  | { kind: 'usage'; usage: SessionUsage }
  | { kind: 'history'; projection: RowProjection }
  | { kind: 'queued'; id: string; text: string }
  | { kind: 'steer'; id: string; text: string; state: SteerState }
  | { kind: 'steer-fallback'; id: string }
  | { kind: 'steers-returned'; ids: ReadonlyArray<string> };

function emptyWindow(sessionId: string): ChatWindow {
  return {
    sessionId,
    cursor: 0,
    projection: newRowProjection(sessionId),
    live: null,
    liveSeq: 0,
    supersededTurn: null,
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
function windowReducer(state: ChatWindow, action: WindowAction): ChatWindow {
  switch (action.kind) {
    case 'reset':
      return emptyWindow(action.sessionId);
    case 'baseline': {
      const { snapshot } = action;
      return {
        ...state,
        sessionId: action.sessionId,
        cursor: snapshot.cursor,
        projection: projectRows(newRowProjection(action.sessionId), snapshot.rows),
        live: snapshot.live.turn_id ? snapshot.live : null,
        liveSeq: snapshot.live.live_seq,
        supersededTurn: null,
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
          if (messages[i].role === 'assistant') {
            messages[i] = { ...messages[i], status };
            break;
          }
        }
        projection = { ...projection, messages };
      }
      const starting = isTurnStart(action.row);
      // A failed turn is a fact the page has to show: the run may
      // have died before any assistant bubble existed.
      const failed = turnErrorMessage(action.row);
      const turnError = failed ?? (starting ? null : state.turnError);
      // Retire the live projection when the durable row now represents
      // it. Every signal is gated on a row type the projection itself
      // renders, so a user / step / usage row can never blank the
      // streaming answer.
      //
      // - the server labelled the row with the live turn (`turn_id`);
      // - an `assistant_message` the server could not label because it
      //   was polled after the run settled, whose text / call ids the
      //   live projection already carries: the durable window renders
      //   that content, so keeping the live item would duplicate it;
      // - a `turn` boundary for a run that is already over.
      let live = state.live;
      let supersededTurn = state.supersededTurn;
      const current = live;
      if (current) {
        const renderedByProjection =
          action.row.type === 'assistant_message' || action.row.type === 'tool_result';
        let retire =
          renderedByProjection && action.turnId !== undefined && action.turnId === current.turn_id;
        if (starting && !current.running) retire = true;
        if (reason !== null && !current.running) retire = true;
        if (!retire && action.row.type === 'assistant_message') {
          const contribution = durableOfMessage(action.row);
          const textHeld = contribution.text.length > 0 && current.text.includes(contribution.text);
          const callsHeld =
            contribution.callIds.length > 0 &&
            contribution.callIds.every((id) =>
              current.tool_calls.some((call) => call.call_id === id),
            );
          if (textHeld || callsHeld) retire = true;
        }
        if (retire) {
          supersededTurn = current.turn_id;
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
        supersededTurn,
      };
    }
    case 'live': {
      const { live } = action;
      if (!live.turn_id) return state;
      // A durable row for this turn already told the truth; the
      // projection is cumulative and would duplicate it.
      if (live.turn_id === state.supersededTurn) return state;
      // A frame at or below the baseline's counter predates the window
      // the client accepted.
      if (live.live_seq <= state.liveSeq) return state;
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
        // A different turn id means a new run; an old supersede mark
        // must not silence it.
        supersededTurn: state.supersededTurn === live.turn_id ? state.supersededTurn : null,
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
function liveSegments(
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
      id: `live-thinking-${live.turn_id}`,
      type: 'thinking',
      content: reasoning,
    });
  }
  if (text) {
    segments.push({ id: `live-text-${live.turn_id}`, type: 'text', content: text });
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
      id: `live-call-${live.turn_id}-${call.call_id}`,
      type: 'tool_block',
      content: '',
      toolName: call.name,
      callContent: callBody,
      ...(call.output !== undefined ? { resultContent: call.output } : {}),
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
function actionErrorText(err: unknown, fallback: string): string {
  if (!(err instanceof ApiError)) {
    return err instanceof Error ? err.message : fallback;
  }
  switch (err.code) {
    case 'resume_conflict':
      return '日志已经变化（resume_conflict）。点击“重新连接并同步”读取最新基线后再提交。';
    case 'resume_no_snapshot':
      return '这一轮不能续（resume_no_snapshot）：日志里没有请求快照，请重开会话。';
    case 'config_drift':
      return '请求配置在挂起后变化（config_drift）。接受变化后重试，或重新连接以刷新。';
    case 'incomplete_resume':
      return '还有未决调用没有决定（incomplete_resume）。';
    case 'session_busy':
      return '会话正在运行（session_busy），请稍后再试。';
    case 'session_suspended':
      return '会话正在等待审批（session_suspended），先处理未决调用。';
    case 'compaction_not_configured':
      return '此部署没有配置压缩策略（compaction_not_configured）。';
    case 'remote_session':
      return '会话由其他节点持有（remote_session）。';
    default:
      return err.message || fallback;
  }
}

/**
 * Main chat page.
 *
 * The transcript is server truth from `GET /api/v1/sessions/{id}/follow`:
 * the first frame replaces the window, durable rows append, and the
 * live projection renders the turn that is still in flight. Sending is a
 * REST action (`queue` or `steer`) whose result arrives through the
 * same stream, so there is one source of truth on every path — no local
 * cache merged onto a server replay.
 */
export function ChatPage() {
  const { sessionId: routeSessionId, agentName: routeAgentName } = useParams<{
    sessionId?: string;
    agentName?: string;
  }>();
  const navigate = useNavigate();

  // `/chat` is the new-conversation view: no session is minted until the
  // user actually sends something. Opening the sidebar's 新会话 (or
  // leaving settings with Esc) therefore does not leave an empty session
  // behind; the id is created by `send()` below, and the follow stream
  // only ever subscribes to a real one.
  const sessionId = routeSessionId;

  /**
   * URL-based agent selection:
   *
   *   /chat/:sessionId                  → resolve the default agent
   *   /chat/:sessionId/agent/:agentName → use the named agent
   *
   * `navigate` is the side-effect: it keeps the URL in sync with the
   * backend's answer, and the same name rides every send.
   */
  const [agentName, setAgentName] = useState<string | null>(routeAgentName ?? null);
  const [agentError, setAgentError] = useState<string | null>(null);
  useEffect(() => {
    if (routeAgentName !== undefined) {
      setAgentName(routeAgentName);
      setAgentError(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const resp = await api.get<{ name: string; source: string }>('/api/v1/agents/default');
        if (cancelled) return;
        setAgentError(null);
        setAgentName(resp.name);
        // The URL only carries the agent once there is a session to
        // carry it; the new-conversation view keeps `/chat`.
        if (sessionId) {
          navigate(`/chat/${sessionId}/agent/${encodeURIComponent(resp.name)}`, {
            replace: true,
          });
        }
      } catch (err) {
        if (cancelled) return;
        setAgentError(err instanceof Error ? err.message : 'No agents registered.');
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [routeAgentName, sessionId, navigate]);

  const [win, dispatch] = useReducer(windowReducer, sessionId ?? '', emptyWindow);

  // A session switch keeps this component instance. The baseline that
  // follows will replace the window anyway, but clearing first stops
  // the previous session's transcript from flashing on the new URL.
  useEffect(() => {
    if (!sessionId) return;
    if (win.sessionId !== sessionId) dispatch({ kind: 'reset', sessionId });
  }, [sessionId, win.sessionId]);

  /**
   * Draft persistence, per session. Lazy initializer so the first paint
   * shows the stored draft without a flicker; debounced so fast typing
   * doesn't thrash disk I/O.
   */
  const [input, setInput] = useState<string>(() => {
    if (!sessionId) return '';
    try {
      return localStorage.getItem(`elysper.draft.${sessionId}`) ?? '';
    } catch {
      return '';
    }
  });
  useEffect(() => {
    if (!sessionId) return;
    if (input === '') {
      try {
        localStorage.removeItem(`elysper.draft.${sessionId}`);
      } catch {
        // quota / private mode
      }
      return;
    }
    const timer = setTimeout(() => {
      try {
        localStorage.setItem(`elysper.draft.${sessionId}`, input);
      } catch {
        // drafts are best-effort
      }
    }, 200);
    return () => clearTimeout(timer);
  }, [input, sessionId]);
  // A session switch (same component instance) reads the other
  // session's draft rather than carrying this one over.
  const draftSessionRef = useRef(sessionId);
  useEffect(() => {
    if (draftSessionRef.current === sessionId) return;
    draftSessionRef.current = sessionId;
    try {
      setInput(sessionId ? (localStorage.getItem(`elysper.draft.${sessionId}`) ?? '') : '');
    } catch {
      setInput('');
    }
  }, [sessionId]);

  const [isConnected, setIsConnected] = useState(false);
  const [followError, setFollowError] = useState<string | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);
  /** One POST in flight at a time: a double-click must not post twice. */
  const [sending, setSending] = useState(false);
  const [decisions, setDecisions] = useState<Record<string, ResumeStatus>>({});
  const [sessionGrants, setSessionGrants] = useState<Record<string, boolean>>({});
  const [resuming, setResuming] = useState(false);
  const [approvalError, setApprovalError] = useState<string | null>(null);
  const [approvalErrorCode, setApprovalErrorCode] = useState<string | null>(null);
  /** Bumped to reopen the follow stream on demand (a resume conflict). */
  const [reconnectNonce, setReconnectNonce] = useState(0);
  const [compacting, setCompacting] = useState(false);

  /**
   * Whether the progress/artifacts side panel is open. Default open —
   * the panel is the at-a-glance view of what the run is doing —
   * persisted so the choice survives a reload.
   */
  const PANEL_OPEN_KEY = 'elysper.chat.panelOpen.v1';
  const [panelOpen, setPanelOpen] = useState(
    () => localStorage.getItem(PANEL_OPEN_KEY) !== 'false',
  );
  useEffect(() => {
    localStorage.setItem(PANEL_OPEN_KEY, String(panelOpen));
  }, [panelOpen]);

  /** Multimodal parts queued for the next submission. */
  const [pendingAttachments, setPendingAttachments] = useState<AttachmentPart[]>([]);
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [selectedModel, setSelectedModel] = useState<string | null>(null);
  const [agents, setAgents] = useState<AgentDetail[]>([]);
  const isServerAvailable = useServerHealth();
  const { push: pushToast } = useToast();
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  /**
   * Whether the thread is pinned to the newest content. Held in a ref
   * *and* mirrored into state: the auto-scroll effect reads it
   * synchronously, while the jump-to-latest affordance needs a render.
   */
  const atBottomRef = useRef(true);
  const [showJumpToLatest, setShowJumpToLatest] = useState(false);

  /**
   * Tick counter that advances every 5 seconds, purely to re-render
   * pending tool blocks so their timeout indicator flips without a
   * setTimeout per call.
   */
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((n) => n + 1), 5_000);
    return () => clearInterval(id);
  }, []);

  // Kick off the markdown chunk on mount so the first assistant reply
  // does not have to wait for the code-highlighting bundle.
  useEffect(() => {
    void Promise.all([
      import('react-markdown'),
      import('remark-gfm'),
      import('rehype-highlight'),
      import('highlight.js/styles/atom-one-light.css'),
    ]);
  }, []);

  // Auto-grow the textarea to its content, clamped by CSS.
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`;
  }, [input]);

  const liveMessageSegments = useMemo(
    () =>
      win.live
        ? liveSegments(win.live, projectedDurable(win.projection), win.liveCallStartedAt)
        : [],
    [win.live, win.projection, win.liveCallStartedAt],
  );

  const viewMessages = useMemo<ChatMessageViewItem[]>(() => {
    const items: ChatMessageViewItem[] = win.projection.messages.map((message) => ({
      id: message.id,
      role: message.role,
      subagentDepth: message.subagentDepth,
      segments: message.segments as MessageSegment[],
      status: message.status,
    }));
    if (win.live && liveMessageSegments.length > 0) {
      items.push({
        id: `live:${win.live.turn_id}`,
        role: 'assistant',
        segments: liveMessageSegments,
        status: win.live.running ? 'working' : 'completed',
        isStreaming: win.live.running,
      });
    }
    return items;
  }, [win.projection, win.live, liveMessageSegments]);

  // Auto-scroll on every content change unless the reader scrolled up.
  useLayoutEffect(() => {
    const list = messagesEndRef.current?.previousElementSibling as HTMLElement | null;
    if (!list || list.scrollHeight <= list.clientHeight) return;
    if (!atBottomRef.current) return;
    list.scrollTop = list.scrollHeight;
  }, [viewMessages, tick]);

  const isEmptyThread = viewMessages.length === 0;
  useEffect(() => {
    const list = messagesEndRef.current?.previousElementSibling as HTMLElement | null;
    if (!list) return;
    const handleScroll = () => {
      const distanceFromBottom = list.scrollHeight - list.scrollTop - list.clientHeight;
      const atBottom = distanceFromBottom <= NEAR_BOTTOM_PX;
      atBottomRef.current = atBottom;
      setShowJumpToLatest((shown) => (shown === !atBottom ? shown : !atBottom));
    };
    list.addEventListener('scroll', handleScroll, { passive: true });
    // Re-pin when the content grows without a React re-render (a
    // lazily-loaded markdown chunk, an image decode, a reflow).
    const pinToTail = () => {
      if (atBottomRef.current) list.scrollTop = list.scrollHeight;
    };
    const contentGrowth = new MutationObserver(pinToTail);
    contentGrowth.observe(list, { childList: true, subtree: true, characterData: true });
    handleScroll();
    return () => {
      list.removeEventListener('scroll', handleScroll);
      contentGrowth.disconnect();
    };
  }, [isEmptyThread, sessionId]);

  const jumpToLatest = useCallback(() => {
    const list = messagesEndRef.current?.previousElementSibling as HTMLElement | null;
    if (!list) return;
    atBottomRef.current = true;
    setShowJumpToLatest(false);
    list.scrollTop = list.scrollHeight;
  }, []);

  // Toast on server-health transitions, not on the initial value.
  const lastHealthRef = useRef(isServerAvailable);
  useEffect(() => {
    if (lastHealthRef.current === isServerAvailable) return;
    lastHealthRef.current = isServerAvailable;
    if (isServerAvailable) {
      pushToast({
        variant: 'success',
        message: 'Elysper backend is back online.',
        durationMs: 3000,
      });
    } else {
      pushToast({
        variant: 'warning',
        message: 'Elysper backend unreachable — sending is disabled.',
        durationMs: 5000,
      });
    }
  }, [isServerAvailable, pushToast]);

  const refreshUsage = useCallback(async (id: string) => {
    try {
      const usage = await getSessionUsage(id);
      dispatch({ kind: 'usage', usage });
    } catch {
      // The footer keeps the last known numbers; the next turn retries.
    }
  }, []);

  /**
   * The follow stream: one long-lived connection, reopened on a drop.
   *
   * Each open starts a fresh baseline, and the reducer replaces the
   * window from it. There is no cursor to persist: the baseline carries
   * the current cursor, and rows after it are the only increments a
   * client ever needs.
   */
  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    let active: AbortController | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined;

    const scheduleReconnect = () => {
      setIsConnected(false);
      reconnectTimer = setTimeout(() => {
        reconnectTimer = undefined;
        if (!cancelled) void open();
      }, RECONNECT_DELAY_MS);
    };

    const open = async () => {
      const controller = new AbortController();
      active = controller;
      try {
        for await (const event of followSession(sessionId, { signal: controller.signal })) {
          if (cancelled) return;
          switch (event.type) {
            case 'baseline':
              setIsConnected(true);
              setFollowError(null);
              dispatch({ kind: 'baseline', sessionId, snapshot: event.snapshot });
              void refreshUsage(sessionId);
              break;
            case 'row': {
              // A request snapshot changed the envelope a resume must
              // reproduce; the token is built from it.
              if (event.hash !== undefined) {
                dispatch({ kind: 'request-hash', hash: event.hash });
              }
              dispatch({
                kind: 'row',
                seq: event.seq,
                row: event.row,
                ...(event.turnId !== undefined ? { turnId: event.turnId } : {}),
              });
              // The follow baseline carries `request_snapshot_hash`;
              // the browser does not recompute it. The token the
              // approval click sends is the one the server issued.
              if (event.row.type === 'assistant_message' || turnEndReason(event.row) !== null) {
                void refreshUsage(sessionId);
              }
              break;
            }
            case 'live':
              dispatch({ kind: 'live', live: event.live, now: Date.now() });
              break;
            case 'interrupts':
              dispatch({ kind: 'interrupts', pending: event.pending });
              break;
          }
        }
        if (!cancelled) scheduleReconnect();
      } catch (err) {
        if (cancelled) return;
        if (err instanceof Error && err.name === 'AbortError') return;
        setFollowError(err instanceof Error ? err.message : String(err));
        // A refused open is not a drop: retrying the same 404/410 can
        // only produce the same answer, so say so and stop.
        if (err instanceof FollowHttpError && (err.status === 404 || err.status === 410)) {
          setIsConnected(false);
          return;
        }
        scheduleReconnect();
      }
    };

    void open();
    return () => {
      cancelled = true;
      active?.abort();
      clearTimeout(reconnectTimer);
    };
  }, [sessionId, refreshUsage, reconnectNonce]);

  // Prune decisions for interrupts that are no longer open (a resume
  // settled them, or a reconnect showed a different set).
  useEffect(() => {
    const openIds = new Set(win.interrupts.map((interrupt) => interrupt.id));
    setDecisions((prev) => {
      const kept = Object.entries(prev).filter(([id]) => openIds.has(id));
      if (kept.length === Object.keys(prev).length) return prev;
      return Object.fromEntries(kept);
    });
    setSessionGrants((prev) => {
      const kept = Object.entries(prev).filter(([id]) => openIds.has(id));
      if (kept.length === Object.keys(prev).length) return prev;
      return Object.fromEntries(kept);
    });
    if (win.interrupts.length === 0) setApprovalError(null);
  }, [win.interrupts]);

  // Fetch the model catalog once per server availability.
  useEffect(() => {
    if (!isServerAvailable) return;
    let cancelled = false;
    apiListModels()
      .then((resp) => {
        if (cancelled) return;
        setModels(resp.models);
        setSelectedModel(`${resp.default_provider}/${resp.default_model}`);
      })
      .catch(() => {
        // Leave the selector hidden; the server resolves its default.
      });
    return () => {
      cancelled = true;
    };
  }, [isServerAvailable]);

  // Fetch the registered agent list once so the composer can offer a
  // dropdown when more than one exists.
  useEffect(() => {
    if (!isServerAvailable) return;
    let cancelled = false;
    api
      .get<List<AgentDetail>>('/api/v1/agents')
      .then((resp) => {
        if (!cancelled) setAgents(resp.data);
      })
      .catch(() => {
        if (!cancelled) setAgents([]);
      });
    return () => {
      cancelled = true;
    };
  }, [isServerAvailable]);

  const awaitingApproval = win.interrupts.length > 0;
  const isStreaming = win.live?.running === true;

  /**
   * Send the composer's content.
   *
   * `queue` is the ordinary path: the message waits its turn and the
   * run logs it when it starts, so the composer shows it as queued
   * until the row arrives. `steer` asks for injection into the live
   * run; the server answers with the delivery that actually happened,
   * and a settled run turns it into the next turn instead of losing it.
   */
  const send = async (delivery: 'queue' | 'steer') => {
    if (!isServerAvailable || awaitingApproval || sending) return;
    const text = input.trim();
    const hasAttachments = pendingAttachments.length > 0;
    if (delivery === 'steer') {
      if (!text || hasAttachments || !isStreaming) return;
    } else if (!text && !hasAttachments) {
      return;
    }
    setSubmitError(null);
    setSending(true);
    // The steer chip is registered BEFORE the request: the server
    // flushes the row before it answers, so a chip added afterwards can
    // miss the row frame that carries its `rowSeq` and stay `accepted`
    // forever (letting `Stop` return an already-injected steer).
    const steerId = delivery === 'steer' ? crypto.randomUUID() : null;
    if (steerId) {
      dispatch({ kind: 'steer', id: steerId, text, state: 'accepted' });
    }
    try {
      // First message of a new conversation: mint the session now (the
      // server owns the id) and send to it. Earlier visits created a
      // session on mount, which left an empty session behind every time
      // `新会话` was clicked.
      const id = sessionId ?? (await createChatSession());
      const response = await sendChatMessage(id, {
        text,
        attachments: delivery === 'queue' ? pendingAttachments : undefined,
        delivery,
        agentName: agentName ?? undefined,
        model: selectedModel ?? undefined,
      });
      if (!sessionId) {
        navigate(agentName ? `/chat/${id}/agent/${encodeURIComponent(agentName)}` : `/chat/${id}`, {
          replace: true,
        });
      }
      setInput('');
      if (delivery === 'queue') {
        setPendingAttachments([]);
        atBottomRef.current = true;
        setShowJumpToLatest(false);
        inputRef.current?.focus();
        dispatch({ kind: 'queued', id: crypto.randomUUID(), text });
        return;
      }
      if (steerId && response.delivery === 'queue') {
        // The run settled first; the message is a queued turn now, so
        // its chip tells the truth rather than promising an injection.
        dispatch({ kind: 'steer-fallback', id: steerId });
      }
      inputRef.current?.focus();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setSubmitError(message);
      if (steerId) dispatch({ kind: 'steers-returned', ids: [steerId] });
      if (!(err instanceof ApiError)) setServerHealth(false);
    } finally {
      setSending(false);
    }
  };

  const handleSubmit = (e?: FormEvent) => {
    e?.preventDefault();
    void send('queue');
  };

  const handleSteer = () => {
    void send('steer');
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      void send('queue');
    }
  };

  /**
   * Stop the run (or settle a suspended one).
   *
   * A steer that never reached a step boundary has no live run left to
   * read it, so it goes back to the editor instead of staying in the
   * log as a message the user cannot see or edit.
   */
  const handleStop = async () => {
    if (!sessionId) return;
    const pending = win.steers.filter((steer) => steer.state === 'accepted');
    if (pending.length > 0) {
      const restored = pending.map((steer) => steer.text).join('\n');
      setInput((prev) => (prev ? `${prev}\n${restored}` : restored));
      dispatch({ kind: 'steers-returned', ids: pending.map((steer) => steer.id) });
    }
    try {
      await apiCancelSession(sessionId);
    } catch {
      // Best-effort — the follow stream is what reports the outcome.
    }
  };

  const decide = useCallback((id: string, status: ResumeStatus) => {
    setDecisions((prev) => ({ ...prev, [id]: status }));
    setSessionGrants((prev) => {
      if (prev[id] === undefined) return prev;
      const next = { ...prev };
      delete next[id];
      return next;
    });
  }, []);

  const grantForSession = useCallback((id: string) => {
    setDecisions((prev) => ({ ...prev, [id]: 'resolved' }));
    setSessionGrants((prev) => ({ ...prev, [id]: true }));
  }, []);

  const resetDecision = useCallback((id: string) => {
    setDecisions((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
    setSessionGrants((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
  }, []);

  /** Open interrupts (session-wide) without a decision yet. */
  const undecidedCount = win.interrupts.filter(
    (interrupt) => decisions[interrupt.id] === undefined,
  ).length;
  const allDecided = win.interrupts.length > 0 && undecidedCount === 0;

  /**
   * Post the decisions for every open interrupt.
   *
   * The token is rebuilt from the follow baseline: the cursor is the
   * log tail the suspension ended at, the pending set is the held
   * calls, and the fingerprint is what the baseline reported. A log
   * that moved since then answers `resume_conflict`, and the message
   * says to reconnect rather than pretending the click worked.
   */
  const submitDecisions = useCallback(
    async (allowDrift = false) => {
      if (!sessionId || !allDecided || resuming) return;
      setResuming(true);
      setApprovalError(null);
      setApprovalErrorCode(null);
      // One decision per open interrupt. A gap here would post a
      // decision object without a status, so the list is built by
      // dropping nothing only when every id resolved.
      const list: ResumeDecision[] = [];
      for (const interrupt of win.interrupts) {
        const status = decisions[interrupt.id];
        if (status === undefined) continue;
        list.push({
          id: interrupt.id,
          status,
          ...(status === 'rejected'
            ? { payload: { reason: 'Rejected in the web client.' } }
            : status === 'resolved' && sessionGrants[interrupt.id]
              ? { payload: { scope: 'session' } }
              : {}),
        });
      }
      if (list.length !== win.interrupts.length) {
        setApprovalError('还有未决调用没有决定（incomplete_resume）。');
        setApprovalErrorCode('incomplete_resume');
        setResuming(false);
        return;
      }
      try {
        await apiResumeSession(
          sessionId,
          {
            last_seq: win.cursor,
            pending: win.interrupts.map((interrupt) => interrupt.call_id),
            request_snapshot_hash: win.requestHash,
          },
          list,
          { allowDrift },
        );
        setDecisions({});
        setSessionGrants({});
        setApprovalErrorCode(null);
      } catch (err) {
        setApprovalError(actionErrorText(err, 'Resume failed.'));
        setApprovalErrorCode(err instanceof ApiError ? err.code : null);
      } finally {
        setResuming(false);
      }
    },
    [
      sessionId,
      allDecided,
      resuming,
      decisions,
      sessionGrants,
      win.interrupts,
      win.cursor,
      win.requestHash,
    ],
  );
  const reconnectFollow = useCallback(() => {
    setApprovalError(null);
    setApprovalErrorCode(null);
    setReconnectNonce((n) => n + 1);
  }, []);

  /** Run one manual compaction pass and report what it did. */
  const handleCompact = async () => {
    if (!sessionId || compacting) return;
    setCompacting(true);
    try {
      const result = await apiCompactSession(sessionId);
      pushToast({
        variant: result.committed ? 'success' : 'warning',
        message: result.committed
          ? `已压缩 ${result.shadowed_messages} 条消息（${result.shadowed_tokens} tokens）。`
          : `未压缩：${result.reason ?? '没有可压缩的跨度'}。`,
        durationMs: 5000,
      });
      void refreshUsage(sessionId);
    } catch (err) {
      pushToast({
        variant: 'error',
        message: actionErrorText(err, 'Compaction failed.'),
        durationMs: 6000,
      });
    } finally {
      setCompacting(false);
    }
  };

  /**
   * Load the full durable transcript.
   *
   * The follow baseline carries a fixed window; the session detail
   * route is the sanctioned way to read further back. This is an
   * explicit replace from server truth, not a local cache spliced onto
   * the stream — the cursor and the live projection are untouched, so
   * subsequent rows still append exactly once.
   */
  const handleLoadHistory = async () => {
    if (!sessionId) return;
    try {
      const detail = await getSessionDetail(sessionId);
      dispatch({
        kind: 'history',
        projection: projectRows(newRowProjection(sessionId), detail.history ?? []),
      });
    } catch (err) {
      pushToast({
        variant: 'error',
        message: err instanceof Error ? err.message : 'Could not load the transcript.',
        durationMs: 5000,
      });
    }
  };

  const handleFeedback = useCallback(
    async (messageId: string, thumbsUp: boolean) => {
      if (!sessionId) return;
      try {
        await apiSubmitFeedback(sessionId, messageId, thumbsUp);
        pushToast({
          variant: 'success',
          message: thumbsUp ? 'Thanks for the feedback 👍' : 'Thanks for the feedback 👎',
          durationMs: 2000,
        });
      } catch (err) {
        pushToast({
          variant: 'error',
          message: `Feedback failed: ${err instanceof Error ? err.message : String(err)}`,
          durationMs: 5000,
        });
      }
    },
    [pushToast, sessionId],
  );

  /**
   * Regenerate the last reply. The route answers `202` and the rerun's
   * rows arrive through `follow`, so there is nothing to reconstruct
   * here — the stream is already the rendering path.
   */
  const handleRegenerate = useCallback(async () => {
    if (!sessionId || isStreaming) return;
    try {
      await apiRegenerate(sessionId);
    } catch (err) {
      pushToast({
        variant: 'error',
        message: `Regenerate failed: ${err instanceof Error ? err.message : String(err)}`,
        durationMs: 5000,
      });
    }
  }, [sessionId, isStreaming, pushToast]);

  // The side panel's aggregates come from the same view the list
  // renders, so the panel and the inline blocks cannot disagree.
  const panelTodos = useMemo(() => deriveTodos(viewMessages), [viewMessages]);
  const panelArtifacts = useMemo(() => deriveArtifacts(viewMessages), [viewMessages]);
  const panelSubagents = useMemo(() => deriveSubagents(viewMessages), [viewMessages]);

  const lastAssistant = useMemo(() => {
    for (let i = viewMessages.length - 1; i >= 0; i -= 1) {
      const message = viewMessages[i];
      if (message.role !== 'assistant') continue;
      // A compaction marker is a timeline note, not an answer: it must
      // never become the target of Regenerate or feedback.
      if (message.segments.every((segment) => segment.type === 'compaction')) continue;
      return message;
    }
    return null;
  }, [viewMessages]);
  const canRegenerate =
    !isStreaming &&
    !awaitingApproval &&
    lastAssistant !== null &&
    lastAssistant.status !== 'working';

  const approvalProps = useMemo<ApprovalCardProps | undefined>(() => {
    if (!awaitingApproval) return undefined;
    return {
      interrupts: win.interrupts,
      decisions,
      grants: sessionGrants,
      onGrant: grantForSession,
      busy: resuming,
      error: approvalError,
      errorCode: approvalErrorCode,
      ready: allDecided,
      undecided: undecidedCount,
      onDecide: decide,
      onReset: resetDecision,
      onSubmit: () => void submitDecisions(),
      onRetryDrift: () => void submitDecisions(true),
      onReconnect: reconnectFollow,
    };
  }, [
    awaitingApproval,
    win.interrupts,
    decisions,
    sessionGrants,
    grantForSession,
    resuming,
    approvalError,
    approvalErrorCode,
    allDecided,
    undecidedCount,
    decide,
    resetDecision,
    submitDecisions,
    reconnectFollow,
  ]);

  // An interrupt whose call is not in the window still needs a card;
  // the inline one is skipped so the call is never approved twice.
  const unboundInterrupts = useMemo(() => {
    if (!awaitingApproval) return [];
    const bound = new Set<string>();
    for (const message of viewMessages) {
      for (const segment of message.segments) {
        if (segment.type === 'tool_block' && segment.toolUseId) bound.add(segment.toolUseId);
      }
    }
    return win.interrupts.filter((interrupt) => !bound.has(interrupt.call_id));
  }, [awaitingApproval, viewMessages, win.interrupts]);

  const contextUsed = win.usage?.context_used ?? win.context?.used;
  const contextWindow = win.usage?.context_window ?? win.context?.window;
  void tick;

  return (
    <div className={`nt-chat-shell${panelOpen ? '' : ' nt-chat-shell--collapsed'}`}>
      <div className="nt-chat">
        <div className="nt-chat__titlebar" data-testid="chat-titlebar">
          <div className="nt-chat__titlebar-left">
            <span className="nt-chat__session-title" data-testid="chat-title">
              {sessionId ? shortId(sessionId) : '新会话'}
            </span>
          </div>
          <div className="nt-chat__titlebar-right">
            <div className="nt-chat__usage-chip" data-testid="usage-chip">
              <span aria-hidden>🧮</span>
              <span className="nt-chat__status-pill" data-testid="session-state">
                {awaitingApproval
                  ? '等待审批'
                  : isStreaming
                    ? '运行中'
                    : isConnected
                      ? '空闲'
                      : followError
                        ? '连接中断'
                        : '连接中…'}
              </span>
            </div>
            {!panelOpen && (
              <button
                type="button"
                className="nt-chat__panel-toggle"
                onClick={() => setPanelOpen(true)}
                data-testid="session-panel-open"
                aria-label="展开进度与产物面板"
              >
                进度与产物
              </button>
            )}
          </div>
        </div>
        {followError && (
          <div className="nt-chat__error-banner" role="alert" aria-live="polite">
            <span className="nt-chat__error-banner-text">
              <strong>实时连接中断：</strong> {followError}（正在重连，重连后会整窗替换）
            </span>
          </div>
        )}
        {viewMessages.length === 0 ? (
          <div className="nt-chat__messages">
            <Card title="System">
              {sessionId ? (
                <p>
                  Welcome to <strong>Elysper</strong>. Type a message below to start a chat.
                  Session: <code>{sessionId.slice(0, 8)}</code>
                </p>
              ) : (
                <p>
                  新会话。<strong>Elysper</strong> 会在你发出第一条消息时创建会话。
                </p>
              )}
            </Card>
          </div>
        ) : (
          <>
            {!win.historyExpanded && (
              <div className="nt-chat__load-history">
                <Button
                  size="1"
                  variant="ghost"
                  color="gray"
                  onClick={() => void handleLoadHistory()}
                  data-testid="load-history-button"
                >
                  加载更早历史
                </Button>
              </div>
            )}
            <ChatMessageList messages={viewMessages} now={Date.now()} approval={approvalProps} />
          </>
        )}
        <div ref={messagesEndRef} aria-hidden />
        {unboundInterrupts.length > 0 && approvalProps && (
          <div className="nt-chat__approval-fallback" data-testid="approval-fallback">
            <ApprovalCard {...approvalProps} interrupts={unboundInterrupts} />
          </div>
        )}
        {showJumpToLatest && (
          <button
            type="button"
            className="nt-chat__jump-latest"
            data-testid="jump-to-latest"
            aria-label="Jump to latest message"
            onClick={jumpToLatest}
          >
            <span aria-hidden>↓</span>
            Jump to latest
          </button>
        )}
        {lastAssistant && canRegenerate && (
          <div className="nt-chat__message-actions" data-testid="message-actions">
            <Button
              size="1"
              variant="soft"
              color="gray"
              onClick={() => void handleRegenerate()}
              data-testid="regenerate-button"
              aria-label="Regenerate response"
            >
              <span aria-hidden>↻</span>
              Regenerate
            </Button>
            <Button
              size="1"
              variant="ghost"
              color="gray"
              onClick={() => void handleFeedback(lastAssistant.id, true)}
              data-testid={`feedback-up-${lastAssistant.id}`}
              aria-label="Mark response as helpful"
            >
              <span aria-hidden>👍</span>
            </Button>
            <Button
              size="1"
              variant="ghost"
              color="gray"
              onClick={() => void handleFeedback(lastAssistant.id, false)}
              data-testid={`feedback-down-${lastAssistant.id}`}
              aria-label="Mark response as unhelpful"
            >
              <span aria-hidden>👎</span>
            </Button>
          </div>
        )}
        {isStreaming && (
          <div className="nt-chat__streaming-indicator" data-testid="typing-dots">
            <span className="nt-chat__typing-dot" />
            <span className="nt-chat__typing-dot" />
            <span className="nt-chat__typing-dot" />
            <span className="nt-chat__typing-label">Thinking…</span>
          </div>
        )}
        {(isStreaming || awaitingApproval) && (
          <button
            type="button"
            aria-label="Stop generating"
            data-testid="stop-button"
            className="nt-chat__stop-button"
            onClick={() => void handleStop()}
          >
            <span aria-hidden style={{ fontSize: 10 }}>
              ■
            </span>
            Stop
          </button>
        )}

        {submitError && (
          <div className="nt-chat__error-banner" role="alert" aria-live="assertive">
            <span className="nt-chat__error-banner-text">
              <strong>发送失败：</strong> {submitError}
            </span>
            <Button size="1" variant="ghost" color="gray" onClick={() => setSubmitError(null)}>
              Dismiss
            </Button>
          </div>
        )}

        <form onSubmit={handleSubmit} className="nt-chat__form">
          {/* The plan the run is executing, above the input. Zero DOM
              when the transcript carries no todo list (review focus 3). */}
          <TodoProgress todos={panelTodos} />
          {agentError && (
            <div
              className="nt-chat__error-banner"
              role="alert"
              aria-live="assertive"
              data-testid="agent-error"
            >
              <span className="nt-chat__error-banner-text">
                <strong>Agent routing unavailable:</strong> {agentError}
              </span>
            </div>
          )}
          {win.turnError && (
            <div
              className="nt-chat__error-banner"
              role="alert"
              aria-live="assertive"
              data-testid="turn-error"
            >
              <span className="nt-chat__error-banner-text">
                <strong>Run failed:</strong> {win.turnError}
              </span>
            </div>
          )}
          {win.queued.length > 0 && (
            <ul className="nt-chat__queued" data-testid="queued-messages">
              {win.queued.map((item) => (
                <li key={item.id} className="nt-chat__queued-item">
                  <span aria-hidden>⏳</span>
                  <span className="nt-chat__queued-text">{item.text || '附件'}</span>
                  <span className="nt-chat__queued-state">排队中</span>
                </li>
              ))}
            </ul>
          )}
          <SteerChips steers={win.steers} />
          {pendingAttachments.length > 0 && (
            <ul className="nt-chat__attachments" data-testid="pending-attachments">
              {pendingAttachments.map((a, idx) => (
                <li
                  key={`${a.kind}-${idx}-${'filename' in a ? (a.filename ?? '') : ''}`}
                  className="nt-chat__attachment"
                >
                  {a.kind === 'image' && a.dataUrl ? (
                    <img
                      src={a.dataUrl}
                      alt={a.filename ?? 'attached image'}
                      className="nt-chat__attachment-thumb"
                    />
                  ) : (
                    <span aria-hidden className="nt-chat__attachment-icon">
                      📎
                    </span>
                  )}
                  <span className="nt-chat__attachment-name">
                    {'filename' in a && a.filename ? a.filename : a.kind}
                  </span>
                  <Button
                    size="1"
                    variant="ghost"
                    color="gray"
                    onClick={() =>
                      setPendingAttachments((prev) => prev.filter((_, i) => i !== idx))
                    }
                    data-testid={`attachment-remove-${idx}`}
                    aria-label="Remove attachment"
                  >
                    ✕
                  </Button>
                </li>
              ))}
            </ul>
          )}
          <div className="nt-chat__input-row">
            <PromptPrefix workspace="默认工作区" context={agentName ?? undefined} />
            <textarea
              ref={inputRef}
              className="nt-chat__input"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={
                awaitingApproval
                  ? '等待审批：先处理上面的未决调用'
                  : 'Type a message... (Enter to send, Shift+Enter for newline)'
              }
              rows={3}
              disabled={!isServerAvailable || awaitingApproval || sending}
              data-testid="chat-input"
              aria-label="Message input"
            />
          </div>
          <div className="nt-chat__composer-bar">
            <label className="nt-chat__attach-button">
              <input
                type="file"
                accept="image/*,application/pdf,text/plain,text/markdown"
                multiple
                disabled={awaitingApproval}
                onChange={async (e: ChangeEvent<HTMLInputElement>) => {
                  const files = Array.from(e.target.files ?? []);
                  if (files.length === 0) return;
                  const added: AttachmentPart[] = [];
                  for (const file of files) {
                    const kind = classifyAttachment(file.type);
                    if (kind === null) {
                      pushToast({
                        variant: 'warning',
                        message: `Skipping ${file.name}: unsupported type ${file.type || 'unknown'}.`,
                        durationMs: 4000,
                      });
                      continue;
                    }
                    try {
                      const dataUrl = await readFileAsDataUrl(file);
                      added.push({
                        kind,
                        dataUrl,
                        mimeType: file.type,
                        filename: file.name,
                      } as AttachmentPart);
                    } catch (err) {
                      pushToast({
                        variant: 'error',
                        message: `Failed to read ${file.name}: ${
                          err instanceof Error ? err.message : String(err)
                        }`,
                        durationMs: 5000,
                      });
                    }
                  }
                  if (added.length > 0) {
                    setPendingAttachments((prev) => [...prev, ...added]);
                  }
                  // Reset so the same file can be re-selected after removal.
                  e.target.value = '';
                }}
                data-testid="attachment-input"
                aria-label="Attach files"
              />
              <span aria-hidden className="nt-chat__attach-glyph">
                +
              </span>
            </label>
            <span
              className="nt-chat__scope-label"
              title="单工作区：服务器只有一个 workspace_root，暂不支持切换"
              data-testid="workspace-scope"
            >
              <span aria-hidden>🕐</span> 默认工作区
            </span>
            {models.length > 0 && (
              <div className="nt-chat__model-selector" data-testid="model-selector">
                <label htmlFor="chat-model-select" className="nt-chat__model-label">
                  Model:
                </label>
                <select
                  id="chat-model-select"
                  className="nt-chat__model-select"
                  value={selectedModel ?? ''}
                  onChange={(e) => setSelectedModel(e.target.value || null)}
                  data-testid="model-select"
                >
                  {models.map((m) => (
                    <option key={`${m.provider}/${m.model}`} value={`${m.provider}/${m.model}`}>
                      {m.provider}/{m.model}
                    </option>
                  ))}
                </select>
              </div>
            )}
            {agentName &&
              (agents.length > 1 ? (
                <div className="nt-chat__agent-selector" data-testid="agent-selector">
                  <label htmlFor="chat-agent-select" className="nt-chat__agent-label">
                    Agent:
                  </label>
                  <select
                    id="chat-agent-select"
                    className="nt-chat__agent-select"
                    value={agentName}
                    onChange={(e) => {
                      if (e.target.value === '') {
                        navigate(`/chat/${sessionId}`);
                      } else {
                        navigate(`/chat/${sessionId}/agent/${encodeURIComponent(e.target.value)}`, {
                          replace: true,
                        });
                      }
                    }}
                    data-testid="agent-select"
                  >
                    <option value="">Default</option>
                    {agents.map((a) => (
                      <option key={a.name} value={a.name}>
                        {a.name}
                      </option>
                    ))}
                  </select>
                </div>
              ) : (
                <div className="nt-chat__agent-chip" data-testid="agent-chip">
                  <span aria-hidden>🤖</span>
                  <span>
                    Agent: <code data-testid="agent-chip-name">{agentName}</code>
                  </span>
                  <Button
                    size="1"
                    variant="ghost"
                    onClick={() => navigate(`/chat/${sessionId}`)}
                    data-testid="agent-clear"
                    aria-label="Clear agent selection"
                  >
                    Clear
                  </Button>
                </div>
              ))}
            {isStreaming && (
              <Button
                type="button"
                size="1"
                variant="soft"
                color="gray"
                className="nt-chat__steer-button"
                disabled={
                  !input.trim() ||
                  pendingAttachments.length > 0 ||
                  awaitingApproval ||
                  !isServerAvailable ||
                  sending
                }
                onClick={handleSteer}
                data-testid="steer-button"
                title={
                  pendingAttachments.length > 0
                    ? '纠偏只承载文本：附件无法注入运行中的回合'
                    : '在当前工具批次结束后注入下一次模型请求'
                }
              >
                Steer
              </Button>
            )}
            <Button
              type="submit"
              className="nt-chat__send-button"
              disabled={
                (!input.trim() && pendingAttachments.length === 0) ||
                !isServerAvailable ||
                awaitingApproval ||
                sending ||
                agentName === null
              }
              data-testid="send-button"
              title={isStreaming ? '排队为下一个回合' : '发送'}
            >
              <span aria-hidden className="nt-chat__send-glyph">
                ↑
              </span>
              <span className="nt-chat__sr-only">{isStreaming ? 'Queue' : 'Send'}</span>
            </Button>
          </div>
          <div className="nt-chat__context-meter" data-testid="context-meter">
            <span>
              上下文 {contextUsed !== undefined ? contextUsed.toLocaleString() : '—'}
              {contextWindow !== undefined ? ` / ${contextWindow.toLocaleString()}` : ''}
            </span>
            <span>
              会话 {win.usage ? win.usage.input_tokens.toLocaleString() : '—'} in ·{' '}
              {win.usage ? win.usage.output_tokens.toLocaleString() : '—'} out ·{' '}
              {win.usage ? win.usage.turns : '—'} turns
            </span>
            <Button
              size="1"
              variant="ghost"
              color="gray"
              onClick={() => void handleCompact()}
              disabled={!isConnected || isStreaming || awaitingApproval || compacting}
              data-testid="compact-button"
              title="手动压缩历史（运行中或挂起时不可用）"
            >
              {compacting ? '压缩中…' : 'Compact'}
            </Button>
          </div>
        </form>
      </div>
      {panelOpen && (
        <SessionSidePanel
          todos={panelTodos}
          artifacts={panelArtifacts}
          subagents={panelSubagents}
          onClose={() => setPanelOpen(false)}
        />
      )}
    </div>
  );
}
