/**
 * The composer's steer states.
 *
 * A steer is a second delivery of user input: instead of waiting in the
 * queue for the current run to settle, it becomes a durable log row
 * that the live run's next step boundary picks up. The user can watch
 * where their correction actually went:
 *
 * - `accepted` — the row is on disk (the response was a durability
 *   promise), but no step has started since.
 * - `injected` — a step boundary after the row carried it into that
 *   step's model request.
 * - `consumed` — the model answered in that step, so the steer did
 *   change the next request.
 * - `next_turn` — the run settled before the boundary, so the server
 *   turned it into an ordinary queued turn. Nothing was lost.
 *
 * `Stop` returns every still-`accepted` steer to the editor instead of
 * leaving it in a log no live run will read.
 */
import type { JSX } from 'react';

export type SteerState = 'accepted' | 'injected' | 'consumed' | 'next_turn';

export interface SteerRecord {
  id: string;
  text: string;
  state: SteerState;
  /** The durable ordinal of the steer's own row, once it is logged. */
  rowSeq?: number;
  /** The step that carried it, once `injected`. */
  step?: number;
}

const STATE_LABEL: Record<SteerState, (steer: SteerRecord) => string> = {
  accepted: () => '已落盘',
  injected: (steer) => `已注入 step ${steer.step ?? '?'}`,
  consumed: () => '已消费',
  next_turn: () => '下一回合',
};

/** The chips sit above the composer: one per steer still worth showing. */
export function SteerChips({ steers }: { steers: ReadonlyArray<SteerRecord> }): JSX.Element | null {
  if (steers.length === 0) return null;
  return (
    <div className="nt-chat__steers" data-testid="steer-chips">
      {steers.map((steer) => (
        <span
          key={steer.id}
          className={`nt-chat__steer nt-chat__steer--${steer.state}`}
          data-testid={`steer-chip-${steer.state}`}
          title={steer.text}
        >
          <span aria-hidden>⤴</span>
          <span className="nt-chat__steer-text">{steer.text}</span>
          <span className="nt-chat__steer-state">{STATE_LABEL[steer.state](steer)}</span>
        </span>
      ))}
    </div>
  );
}
