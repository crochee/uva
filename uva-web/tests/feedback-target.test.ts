/**
 * The feedback target: the ordinal of the rated row.
 *
 * Seam: `src/lib/session-to-messages.ts` projects durable session-log
 * rows into the chat UI's message list, and `src/api/chat-stream.ts`
 * sends the rating. It catches a rating that names something the
 * server cannot resolve — the target must be the row's ordinal in the
 * log, because the server resolves the target against the log's own
 * rows. It misses everything about the server that resolves it.
 *
 * The expected values are worked examples: a literal row and the
 * ordinal the log stamped on it. They are never recomputed by calling
 * the module under test.
 */
import { describe, expect, it, vi } from 'vitest';

import { submitFeedback } from '../src/api/chat-stream';
import type { SessionTurn } from '../src/api/types';
import {
  feedbackTarget,
  newRowProjection,
  projectRows,
  selectLastAssistant,
} from '../src/lib/session-to-messages';

/** The calls the stubbed client recorded. */
const { sent } = vi.hoisted(() => ({
  sent: [] as Array<{ method: string; params: unknown }>,
}));

vi.mock('../src/api/rpc', () => ({
  rpc: {
    request: (method: string, params?: unknown) => {
      sent.push({ method, params });
      return Promise.resolve(null);
    },
  },
  RpcError: class RpcError extends Error {},
}));

/** One typed assistant row, as the log stamps it. */
function assistantRow(seq: number | undefined, text: string): SessionTurn {
  return {
    type: 'assistant_message',
    ...(seq === undefined ? {} : { seq }),
    data: { role: 'assistant', content: [{ type: 'text', text }] },
  };
}

/** One typed user row, as the log stamps it. */
function userRow(seq: number, text: string): SessionTurn {
  return {
    type: 'user_message',
    seq,
    data: { role: 'user', content: [{ type: 'text', text }] },
  };
}

/** One assistant row that carries only reasoning — no text part, no
 * tool call: the model's turn ended before it wrote an answer. */
function reasoningRow(seq: number | undefined, text: string): SessionTurn {
  return {
    type: 'assistant_message',
    ...(seq === undefined ? {} : { seq }),
    data: { role: 'assistant', content: [{ type: 'reasoning', text }] },
  };
}

/** Fold a window of rows onto a fresh projection. */
function fold(rows: ReadonlyArray<SessionTurn>) {
  return projectRows(newRowProjection('sess-1'), rows);
}

describe('the projected message carries its row’s ordinal', () => {
  it('stamps the assistant row’s ordinal on the answer it produced', () => {
    const state = fold([assistantRow(7, 'the answer')]);
    expect(state.messages[0].ordinal).toBe(7);
  });

  it('takes the newest answer’s ordinal when a later answer joins the bubble', () => {
    // One bubble, one rating target: the newest assistant row it
    // holds — the same ordinal the TUI's `resume_material` names, so
    // a thumbs-up lands on the same line in both surfaces.
    const state = fold([assistantRow(7, 'the answer'), assistantRow(9, 'and more')]);
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0].ordinal).toBe(9);
  });

  it('keeps the answer’s ordinal when a later row carries only reasoning', () => {
    // A reasoning-only row is a real log row, but it renders no line
    // in the TUI, so the TUI's `resume_material` still names the
    // answer before it. The web must name the same row — naming the
    // reasoning-only row would rate a turn that wrote no answer.
    const state = fold([assistantRow(7, 'the answer'), reasoningRow(9, 'still thinking')]);
    expect(state.messages).toHaveLength(1);
    expect(state.messages[0].ordinal).toBe(7);
  });

  it('offers no target for a bubble whose only content is reasoning', () => {
    const state = fold([reasoningRow(9, 'still thinking')]);
    expect(feedbackTarget(selectLastAssistant(state.messages))).toBeNull();
  });

  it('offers no target when the current turn wrote only reasoning', () => {
    // A rating is about this turn's answer: the earlier turn's answer
    // belongs to a different turn, so the target must not reach back
    // into it. The turn wrote no answer of its own — it holds only
    // reasoning — so it names no row at all. The TUI's
    // `resume_material` names no row in the same case.
    const state = fold([
      userRow(4, 'hello'),
      assistantRow(5, 'the answer'),
      userRow(6, 'again'),
      reasoningRow(9, 'still thinking'),
    ]);
    expect(feedbackTarget(selectLastAssistant(state.messages))).toBeNull();
  });

  it('stamps nothing on a row the log stamped with no ordinal', () => {
    const state = fold([assistantRow(undefined, 'the answer')]);
    expect(state.messages[0].ordinal).toBeUndefined();
  });

  it('stamps nothing on a user turn', () => {
    const state = fold([userRow(3, 'ask')]);
    expect(state.messages[0].ordinal).toBeUndefined();
  });
});

describe('feedbackTarget', () => {
  it('is the ordinal of the row the answer came from', () => {
    const state = fold([assistantRow(7, 'the answer')]);
    expect(feedbackTarget(state.messages[0])).toBe('7');
  });

  it('is null for a message no assistant row opened', () => {
    const state = fold([assistantRow(undefined, 'the answer')]);
    expect(feedbackTarget(state.messages[0])).toBeNull();
  });

  it('is null when there is no message at all', () => {
    expect(feedbackTarget(null)).toBeNull();
    expect(feedbackTarget(undefined)).toBeNull();
  });
});

describe('submitFeedback', () => {
  it('sends the ordinal as the rating’s target', async () => {
    const state = fold([assistantRow(7, 'the answer')]);
    const target = feedbackTarget(state.messages[0]);
    expect(target).toBe('7');
    if (target === null) throw new Error('the answer names a row');

    sent.length = 0;
    await submitFeedback('sess-1', target, true);

    expect(sent).toEqual([
      {
        method: 'chat/feedback',
        params: { message_id: '7', thumbs_up: true, session_id: 'sess-1' },
      },
    ]);
  });
});

describe('the policy block and the rating target', () => {
  /** One `hook_block` row, as the log stamps it. */
  function hookBlockRow(seq: number): SessionTurn {
    return {
      type: 'hook_block',
      seq,
      data: {
        hook: 'echo-blocker',
        reason: 'echo is not allowed here',
        tool: 'echo',
        call_id: 'c1',
      },
    };
  }

  it('never offers the policy block as a rating target', () => {
    const state = fold([assistantRow(7, 'the answer'), hookBlockRow(8)]);
    const target = feedbackTarget(selectLastAssistant(state.messages));
    expect(target).toBe('7');
  });

  it('offers no target at all when only a policy block was folded', () => {
    const state = fold([hookBlockRow(8)]);
    expect(feedbackTarget(selectLastAssistant(state.messages))).toBeNull();
  });
});
