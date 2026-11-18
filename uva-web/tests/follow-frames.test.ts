/**
 * The four validators of the session follow stream.
 *
 * Seam: `src/api/follow-frames.ts` decides whether a frame the server
 * sent is a frame the client may act on. It catches a malformed frame
 * being accepted (a cursor shifted by a junk row, a tool call keyed by
 * a non-string id, an interrupt without a tool name) and a valid frame
 * being rejected (an increment dropped from the window). It misses
 * everything about whether the server ever sends such a frame.
 *
 * Expectations here are worked examples built from known wire shapes
 * (a literal frame, a hand-worked answer). They are never recomputed
 * by calling the module under test.
 */
import { describe, expect, it } from 'vitest';

import { readBaseline, readInterrupts, readLiveTurn, readRowFrame } from '../src/api/follow-frames';

/** One durable row, as the follow stream carries it. */
const ROW = {
  type: 'assistant_message',
  ts: '2026-11-14T00:00:00Z',
  seq: 12,
  data: {
    role: 'assistant',
    content: { Single: { type: 'text', text: 'done' } },
    tool_call_id: null,
    name: null,
    tool_result_cleared_at: null,
  },
};

/** One call a suspended run is waiting on. */
const INTERRUPT = {
  id: 'int-1',
  call_id: 'call-1',
  tool: 'shell',
  arguments_hash: 'sha256:abc',
  risk: 'low',
  agent: 'worker',
};

/** The session facts the baseline carries. */
const HEADER = { session_id: 's-1', user_id: 'u-1', state: 'running' };

/** The transient projection of the turn that is live right now. */
const LIVE = {
  start_seq: 12,
  live_seq: 3,
  running: true,
  text: 'working',
  reasoning: '',
  tool_calls: [],
};

/** How much of the model's window the next request would occupy. */
const CONTEXT = { used: 1200, window: 8000 };

describe('readRowFrame', () => {
  it('keeps the fields the client acts on', () => {
    expect(
      readRowFrame({
        seq: 12,
        row: ROW,
        request_snapshot_hash: 'hash-1',
      }),
    ).toStrictEqual({
      type: 'row',
      seq: 12,
      row: ROW,
      hash: 'hash-1',
    });
  });

  it('leaves out the optional hash the frame does not carry', () => {
    expect(readRowFrame({ seq: 12, row: ROW })).toStrictEqual({
      type: 'row',
      seq: 12,
      row: ROW,
    });
  });

  it('leaves out an envelope hash that is empty', () => {
    expect(readRowFrame({ seq: 12, row: ROW, request_snapshot_hash: '' })).toStrictEqual({
      type: 'row',
      seq: 12,
      row: ROW,
    });
  });

  it('drops a frame that has no row', () => {
    expect(readRowFrame({ seq: 12 })).toBeNull();
  });

  it('drops a frame whose seq is not a number', () => {
    expect(readRowFrame({ seq: '12', row: ROW })).toBeNull();
  });

  it('drops a frame whose row is not an object', () => {
    expect(readRowFrame({ seq: 12, row: [ROW] })).toBeNull();
  });

  it('drops a frame whose row is a bare value', () => {
    expect(readRowFrame({ seq: 12, row: 'text' })).toBeNull();
  });
});

describe('readInterrupts', () => {
  it('keeps the calls the run is waiting on', () => {
    expect(readInterrupts({ pending_interrupts: [INTERRUPT] })).toStrictEqual([INTERRUPT]);
  });

  it('keeps an empty set of pending calls', () => {
    expect(readInterrupts({ pending_interrupts: [] })).toStrictEqual([]);
  });

  it('drops a frame that has no pending list', () => {
    expect(readInterrupts({ kind: 'interrupts' })).toBeNull();
  });

  it('drops a frame whose pending list is not a list', () => {
    expect(readInterrupts({ pending_interrupts: 'none' })).toBeNull();
  });

  it('drops the call whose fields are not all there', () => {
    expect(
      readInterrupts({
        pending_interrupts: [
          { id: 'int-2', call_id: 'call-2', arguments_hash: 'sha256:def', risk: 'low', agent: 'w' },
        ],
      }),
    ).toStrictEqual([]);
  });

  it('drops the call whose risk is not one the client knows', () => {
    expect(
      readInterrupts({ pending_interrupts: [{ ...INTERRUPT, risk: 'medium' }] }),
    ).toStrictEqual([]);
  });

  it('drops the broken call and keeps the sound one', () => {
    expect(
      readInterrupts({ pending_interrupts: [INTERRUPT, { ...INTERRUPT, id: 5 }] }),
    ).toStrictEqual([INTERRUPT]);
  });

  it('drops a pending call that is not an object', () => {
    expect(readInterrupts({ pending_interrupts: [null, INTERRUPT] })).toStrictEqual([INTERRUPT]);
  });
});

describe('readBaseline', () => {
  it('keeps the window the baseline establishes', () => {
    expect(
      readBaseline({
        header: HEADER,
        rows: [ROW],
        cursor: 12,
        live: LIVE,
        context: CONTEXT,
      }),
    ).toStrictEqual({
      header: HEADER,
      rows: [ROW],
      cursor: 12,
      live: LIVE,
      context: CONTEXT,
      pending_interrupts: [],
      request_snapshot_hash: '',
    });
  });

  it('keeps the pending interrupts and the envelope hash the baseline carries', () => {
    const frame = readBaseline({
      header: HEADER,
      rows: [ROW],
      cursor: 12,
      live: LIVE,
      context: CONTEXT,
      pending_interrupts: [INTERRUPT],
      request_snapshot_hash: 'hash-1',
    });
    expect(frame?.pending_interrupts).toStrictEqual([INTERRUPT]);
    expect(frame?.request_snapshot_hash).toBe('hash-1');
  });

  it('drops a baseline that has no cursor', () => {
    expect(readBaseline({ header: HEADER, rows: [ROW], live: LIVE, context: CONTEXT })).toBeNull();
  });

  it('drops a baseline whose cursor is not a number', () => {
    expect(
      readBaseline({ header: HEADER, rows: [ROW], cursor: '12', live: LIVE, context: CONTEXT }),
    ).toBeNull();
  });

  it('drops a baseline whose rows are not a list', () => {
    expect(
      readBaseline({ header: HEADER, rows: { 0: ROW }, cursor: 12, live: LIVE, context: CONTEXT }),
    ).toBeNull();
  });

  it('drops a baseline whose header is not an object', () => {
    expect(
      readBaseline({ header: 's-1', rows: [ROW], cursor: 12, live: LIVE, context: CONTEXT }),
    ).toBeNull();
  });

  it('drops a baseline whose live projection is not an object', () => {
    expect(
      readBaseline({ header: HEADER, rows: [ROW], cursor: 12, live: 'none', context: CONTEXT }),
    ).toBeNull();
  });

  it('drops a baseline whose context meter is not an object', () => {
    expect(
      readBaseline({ header: HEADER, rows: [ROW], cursor: 12, live: LIVE, context: 'none' }),
    ).toBeNull();
  });
});

describe('readLiveTurn', () => {
  it('keeps the text and the calls of the live turn', () => {
    expect(
      readLiveTurn({
        start_seq: 12,
        live_seq: 3,
        running: true,
        text: 'working',
        reasoning: 'thinking',
        tool_calls: [
          { call_id: 'call-1', name: 'shell', status: 'running', arguments: { cmd: 'ls' } },
        ],
      }),
    ).toStrictEqual({
      start_seq: 12,
      live_seq: 3,
      running: true,
      text: 'working',
      reasoning: 'thinking',
      tool_calls: [
        { call_id: 'call-1', name: 'shell', arguments: { cmd: 'ls' }, status: 'running' },
      ],
    });
  });

  it('fills in the empty text of a turn that has none', () => {
    expect(readLiveTurn({ start_seq: 12, live_seq: 3 })).toStrictEqual({
      start_seq: 12,
      live_seq: 3,
      running: false,
      text: '',
      reasoning: '',
      tool_calls: [],
    });
  });

  it('drops a live frame that has no start position', () => {
    expect(readLiveTurn({ live_seq: 3 })).toBeNull();
  });

  it('drops a live frame whose start position is not a number', () => {
    expect(readLiveTurn({ start_seq: '12', live_seq: 3 })).toBeNull();
  });

  it('drops a live frame whose live seq is not a number', () => {
    expect(readLiveTurn({ start_seq: 12, live_seq: '3' })).toBeNull();
  });

  it('drops the tool call whose status is not one the client knows', () => {
    const frame = readLiveTurn({
      start_seq: 12,
      live_seq: 3,
      tool_calls: [{ call_id: 'call-1', name: 'shell', status: 'paused' }],
    });
    expect(frame?.tool_calls).toStrictEqual([]);
  });

  it('drops the tool call that has no name', () => {
    const frame = readLiveTurn({
      start_seq: 12,
      live_seq: 3,
      tool_calls: [{ call_id: 'call-1', status: 'running' }],
    });
    expect(frame?.tool_calls).toStrictEqual([]);
  });

  it('keeps no tool calls when the frame carries no list', () => {
    const frame = readLiveTurn({ start_seq: 12, live_seq: 3, tool_calls: 'none' });
    expect(frame?.tool_calls).toStrictEqual([]);
  });

  it('keeps no tool calls when the frame carries a bare value', () => {
    const frame = readLiveTurn({ start_seq: 12, live_seq: 3, tool_calls: 7 });
    expect(frame?.tool_calls).toStrictEqual([]);
  });

  it('drops a tool call that is not an object', () => {
    const frame = readLiveTurn({
      start_seq: 12,
      live_seq: 3,
      tool_calls: [null, { call_id: 'call-1', name: 'shell', status: 'running' }],
    });
    expect(frame?.tool_calls).toStrictEqual([
      { call_id: 'call-1', name: 'shell', arguments: null, status: 'running' },
    ]);
  });
});
