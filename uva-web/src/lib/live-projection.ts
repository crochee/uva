/**
 * The live projection's supersession rule.
 *
 * The server projects one **step** at a time: the transient text,
 * reasoning and tool calls a client renders while the model streams.
 * The projection carries the log position where its step began
 * (`start_seq`), and the durable row that carries the step's own
 * content retires it.
 *
 * The rule is an ordinal comparison, not an id match and not a text
 * comparison: a content row whose ordinal is greater than the
 * projection's start position is that step's answer, so the projection
 * has nothing left to render.
 *
 * This module owns the **whole** retirement question, not half of it:
 *
 * - a content row (assistant message / tool result) past the
 *   projection's start position retires it;
 * - a turn boundary row retires it once the run is over — nothing
 *   else will replace it, and no content row is coming;
 * - a live frame for a projection that was already retired may not
 *   come back.
 *
 * A caller asks; it does not re-decide. The conditions live here
 * because ADR-0012 settled them as **protocol**: the ordinal rule and
 * the boundary rule are one rule, and both clients answer it the same
 * way.
 */

import { boundaryKind } from './session-to-messages';
import type { SessionTurn } from '../api/types';

/**
 * The projection's identity and liveness at the moment of the
 * question.
 */
export interface ProjectionPosition {
  /** The log position where the projection's step began. */
  startSeq: number;
  /** Whether the projection's own step is still running. */
  running: boolean;
}

/**
 * The boundary kind of a turn row (`start` / `end`), or the empty
 * string for anything that is not a turn boundary.
 */
function turnBoundary(row: SessionTurn): string {
  if (row.type !== 'turn') return '';
  return boundaryKind(row);
}

/**
 * Whether a durable row retires the live projection.
 *
 * Two conditions, both settled by ADR-0012:
 *
 * - the row carries the projection's own content — a content row (an
 *   assistant message or a tool result) whose ordinal is greater than
 *   the projection's start position is that step's answer, so keeping
 *   the live item would duplicate it. The user prompt, the turn and
 *   step boundaries, the usage frame and the approvals are not what
 *   the projection renders, and retiring it on those would blank the
 *   streaming answer;
 * - the row is a turn boundary and the run is over — nothing else
 *   will replace the projection.
 *
 * The question is asked of the **published** row, so a row written in
 * an older dialect answers it too: the server publishes current rows
 * only.
 */
export function retiresProjection(
  row: SessionTurn,
  seq: number,
  position: ProjectionPosition,
): boolean {
  const isContentRow = row.type === 'assistant_message' || row.type === 'tool_result';
  if (isContentRow && seq > position.startSeq) return true;
  const boundary = turnBoundary(row);
  if (boundary !== 'start' && boundary !== 'end') return false;
  return !position.running;
}

/**
 * Whether a live frame may still paint the projection.
 *
 * A frame for a projection the durable rows already retired must not
 * come back; a later step's position means a newer projection, and
 * the mark of an older retired one must not silence it.
 */
export function liveFrameAdmitted(startSeq: number, supersededStart: number | null): boolean {
  if (supersededStart === null) return true;
  return startSeq > supersededStart;
}
