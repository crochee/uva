/**
 * Turning a profile transaction's receipt into something a row can
 * show.
 *
 * The four states are not interchangeable and must never be collapsed
 * into "saved": `overridden` and `failed` wrote nothing, and
 * `restart-required` needs the operator to restart before the change
 * is live. The copy here is the one place that wording lives, so the
 * eight sections cannot drift.
 */
import type { ProfileChange, ProfileChangeState } from '../api/settings-profile';

export type FeedbackTone = 'applied' | 'warn' | 'error';

export interface RowFeedback {
  tone: FeedbackTone;
  text: string;
}

const PREFIX: Record<ProfileChangeState, string> = {
  applied: '已应用',
  'restart-required': '已写入 · 重启后生效',
  overridden: '未写入 · 被上层覆盖',
  failed: '失败',
};

const TONE: Record<ProfileChangeState, FeedbackTone> = {
  applied: 'applied',
  'restart-required': 'warn',
  overridden: 'warn',
  failed: 'error',
};

export function receiptOf(change: ProfileChange): RowFeedback {
  const detail = change.reasons[0];
  return {
    tone: TONE[change.state] ?? 'error',
    text: detail ? `${PREFIX[change.state]}：${detail}` : PREFIX[change.state],
  };
}

/** A transport / RPC failure is its own receipt: the transaction never
 *  reached a verdict, so it is reported as a failure with the error's
 *  own words. */
export function failureReceipt(error: unknown): RowFeedback {
  return {
    tone: 'error',
    text: `失败：${error instanceof Error ? error.message : String(error)}`,
  };
}
