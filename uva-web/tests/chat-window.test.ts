import { describe, expect, it } from 'vitest';

import {
  emptyWindow,
  turnEndReason,
  windowReducer,
  type ChatWindow,
  type WindowAction,
} from '../src/lib/chat-window';
import type {
  ApprovalRequest,
  Content,
  FollowSnapshot,
  LiveTurn,
  Message,
  SessionTurn,
  SessionUsage,
} from '../src/api/types';

/**
 * The window read model's own tests.
 *
 * The window is what the chat page renders; the reducer is the only
 * thing that moves it. Every action gets a case here, because the
 * page can no longer be the test surface for any of them.
 */

/** A provider message, built through the one shape the contract publishes. */
function providerMessage(role: Message['role'], content: Content): Message {
  return {
    role,
    content,
    tool_call_id: null,
    name: null,
    tool_result_cleared_at: null,
  };
}

function assistantRow(seq: number, text: string): SessionTurn {
  return {
    type: 'assistant_message',
    seq,
    data: providerMessage('assistant', { Multi: [{ type: 'text', text }] }),
  };
}

function userRow(seq: number, text: string): SessionTurn {
  return {
    type: 'user_message',
    seq,
    data: providerMessage('user', { Multi: [{ type: 'text', text }] }),
  };
}

function turnRow(seq: number, kind: 'start' | 'end', reason?: string): SessionTurn {
  return { type: 'turn', seq, data: reason === undefined ? { kind } : { kind, reason } };
}

function liveFrame(overrides: Partial<LiveTurn> = {}): LiveTurn {
  return {
    start_seq: 10,
    live_seq: 1,
    running: true,
    text: 'streaming',
    reasoning: '',
    tool_calls: [],
    ...overrides,
  };
}

function snapshot(overrides: Partial<FollowSnapshot> = {}): FollowSnapshot {
  return {
    header: {
      session_id: 'sess-1',
      user_id: 'user-1',
      state: 'working',
      created_at: '2026-11-18T00:00:00Z',
      updated_at: '2026-11-18T00:00:00Z',
    },
    rows: [],
    cursor: 0,
    live: liveFrame({ live_seq: 0, running: false }),
    context: { used: 0, window: 1000 },
    pending_interrupts: [],
    request_snapshot_hash: '',
    ...overrides,
  };
}

function apply(state: ChatWindow, action: WindowAction): ChatWindow {
  return windowReducer(state, action);
}

describe('the window reducer', () => {
  it('resets to an empty window', () => {
    const state = apply(emptyWindow('sess-1'), { kind: 'reset', sessionId: 'sess-2' });
    expect(state.sessionId).toBe('sess-2');
    expect(state.cursor).toBe(0);
    expect(state.live).toBeNull();
  });

  it('replaces the window on a baseline', () => {
    const state = apply(emptyWindow('sess-1'), {
      kind: 'baseline',
      sessionId: 'sess-1',
      snapshot: snapshot({
        rows: [assistantRow(1, 'hello')],
        cursor: 1,
        request_snapshot_hash: 'hash-1',
      }),
    });
    expect(state.cursor).toBe(1);
    expect(state.requestHash).toBe('hash-1');
    expect(state.projection.messages.length).toBe(1);
  });

  it('appends a row past the cursor and ignores one at or below it', () => {
    const first = apply(emptyWindow('sess-1'), { kind: 'row', seq: 1, row: assistantRow(1, 'hi') });
    expect(first.cursor).toBe(1);
    expect(first.projection.messages.length).toBe(1);

    const duplicate = apply(first, { kind: 'row', seq: 1, row: assistantRow(1, 'hi') });
    expect(duplicate).toBe(first);
  });

  it('retires the live projection when its own content row lands', () => {
    const live = apply(emptyWindow('sess-1'), {
      kind: 'live',
      live: liveFrame({ start_seq: 10 }),
      now: 1000,
    });
    expect(live.live).not.toBeNull();

    const retired = apply(live, { kind: 'row', seq: 11, row: assistantRow(11, 'the answer') });
    expect(retired.live).toBeNull();
    expect(retired.supersededStart).toBe(10);
  });

  it('keeps the projection when the row is not its content', () => {
    const live = apply(emptyWindow('sess-1'), {
      kind: 'live',
      live: liveFrame({ start_seq: 10 }),
      now: 1000,
    });
    const kept = apply(live, { kind: 'row', seq: 11, row: userRow(11, 'another prompt') });
    expect(kept.live).not.toBeNull();
  });

  it('rejects a live frame for a projection already retired', () => {
    const live = apply(emptyWindow('sess-1'), {
      kind: 'live',
      live: liveFrame({ start_seq: 10 }),
      now: 1000,
    });
    const retired = apply(live, { kind: 'row', seq: 11, row: assistantRow(11, 'the answer') });
    const revived = apply(retired, {
      kind: 'live',
      live: liveFrame({ start_seq: 10, live_seq: 2 }),
      now: 2000,
    });
    expect(revived).toBe(retired);
  });

  it('rejects a live frame older than the last accepted one', () => {
    const live = apply(emptyWindow('sess-1'), {
      kind: 'live',
      live: liveFrame({ start_seq: 10, live_seq: 5 }),
      now: 1000,
    });
    const stale = apply(live, {
      kind: 'live',
      live: liveFrame({ start_seq: 10, live_seq: 4 }),
      now: 2000,
    });
    expect(stale).toBe(live);
  });

  it('stamps the first-seen time of a live tool call', () => {
    const call = {
      call_id: 'call-1',
      name: 'shell',
      arguments: { command: 'ls' },
      status: 'running',
    };
    const live = apply(emptyWindow('sess-1'), {
      kind: 'live',
      live: liveFrame({ tool_calls: [call] }),
      now: 1000,
    });
    expect(live.liveCallStartedAt['call-1']).toBe(1000);
  });

  it('carries the interrupts, the request hash and the usage', () => {
    const pending: ApprovalRequest[] = [
      {
        id: 'ask-1',
        call_id: 'call-1',
        tool: 'shell',
        arguments_hash: 'hash',
        risk: 'unknown',
        agent: 'worker',
      },
    ];
    const usage: SessionUsage = {
      session_id: 'sess-1',
      input_tokens: 10,
      output_tokens: 20,
      turns: 1,
      context_used: 30,
    };
    let state = apply(emptyWindow('sess-1'), { kind: 'interrupts', pending });
    expect(state.interrupts).toHaveLength(1);

    state = apply(state, { kind: 'request-hash', hash: 'hash-1' });
    expect(state.requestHash).toBe('hash-1');

    state = apply(state, { kind: 'usage', usage });
    expect(state.usage).toEqual(usage);
  });

  it('replaces the projection with the full history when asked', () => {
    const state = apply(emptyWindow('sess-1'), {
      kind: 'history',
      projection: emptyWindow('sess-1').projection,
    });
    expect(state.historyExpanded).toBe(true);
  });

  it('queues a message and settles it when the prompt row lands', () => {
    const queued = apply(emptyWindow('sess-1'), { kind: 'queued', id: 'q-1', text: 'run it' });
    expect(queued.queued).toHaveLength(1);

    const settled = apply(queued, { kind: 'row', seq: 1, row: userRow(1, 'run it') });
    expect(settled.queued).toHaveLength(0);
  });

  it('holds a steer chip, falls it back to the next turn, and drops it when it returns', () => {
    let state = apply(emptyWindow('sess-1'), {
      kind: 'steer',
      id: 'steer-1',
      text: 'also check the logs',
      state: 'accepted',
    });
    expect(state.steers).toHaveLength(1);

    state = apply(state, { kind: 'steer-fallback', id: 'steer-1' });
    expect(state.steers[0].state).toBe('next_turn');

    state = apply(state, { kind: 'steers-returned', ids: ['steer-1'] });
    expect(state.steers).toHaveLength(0);
  });

  it('drops the steer chips whose turn is over', () => {
    let state = apply(emptyWindow('sess-1'), {
      kind: 'steer',
      id: 'steer-1',
      text: 'also check the logs',
      state: 'accepted',
    });
    state = apply(state, { kind: 'row', seq: 1, row: turnRow(1, 'end', 'completed') });
    expect(state.steers).toHaveLength(0);
  });
});

describe("a turn boundary is read from the row's typed member", () => {
  it('reads the typed `kind` the row declares', () => {
    // The row's typed member is the fact: the writer stamps `kind` on
    // the row's top level and the contract declares it there. The
    // payload bag's copy is the older spelling.
    const row: SessionTurn = {
      type: 'turn',
      seq: 1,
      kind: 'end',
      data: { reason: 'completed' },
    };
    expect(turnEndReason(row)).toBe('completed');
  });

  it('still reads a row written before the member was typed', () => {
    const row: SessionTurn = {
      type: 'turn',
      seq: 1,
      data: { kind: 'end', reason: 'completed' },
    };
    expect(turnEndReason(row)).toBe('completed');
  });

  it("prefers the typed member over the payload bag's copy", () => {
    const row: SessionTurn = {
      type: 'turn',
      seq: 1,
      kind: 'end',
      data: { kind: 'start' },
    };
    expect(turnEndReason(row)).toBe('completed');
  });
});
