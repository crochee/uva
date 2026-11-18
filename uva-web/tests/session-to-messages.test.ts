/**
 * The session-log → chat-message projection.
 *
 * Seam: `src/lib/session-to-messages.ts` turns durable session-log rows
 * into the chat UI's message list. It catches a change in how a row
 * becomes a UI message — the published row shapes, the tool-block
 * pairing, the surface replace op. It misses everything about the
 * server that writes those rows.
 *
 * Expectations here are worked examples built from known row shapes
 * (a literal row, a hand-worked answer). They are never recomputed by
 * calling the module under test.
 */
import { describe, expect, it } from 'vitest';

import type { Content, Message, SessionDetail, SessionTurn } from '../src/api/types';
import type { ChatMessageLike } from '../src/lib/session-to-messages';
import {
  carriesModelContent,
  firstUserPromptText,
  isToolEchoText,
  mergeReconstructedMessages,
  messageParts,
  newRowProjection,
  projectRows,
  reconstructMessagesFromSession,
  selectLastAssistant,
  feedbackTarget,
} from '../src/lib/session-to-messages';

/** A user's bubble, as the chat UI holds it. */
function userMessage(text: string): ChatMessageLike {
  return { id: 'user-1', role: 'user', segments: [{ id: 'seg-1', type: 'text', content: text }] };
}

/** An assistant's bubble, optionally bound to a session. */
function assistantMessage(text: string, sessionId?: string): ChatMessageLike {
  return {
    id: 'assistant-1',
    role: 'assistant',
    segments: [{ id: 'seg-1', type: 'text', content: text }],
    ...(sessionId === undefined ? {} : { sessionId }),
  };
}

/** The comparable shape of a message list — ids are random, so they stay out. */
function outline(messages: ReadonlyArray<ChatMessageLike>) {
  return messages.map((message) => ({
    role: message.role,
    segments: message.segments.map((segment) => ({
      type: segment.type,
      content: segment.content,
      toolName: segment.toolName,
      callContent: segment.callContent,
      resultContent: segment.resultContent,
      toolPending: segment.toolPending,
      toolError: segment.toolError,
      toolUseId: segment.toolUseId,
    })),
  }));
}

/** Fold a window of rows onto a fresh projection. Fixtures are
 *  written as the wire spells them: the server publishes only
 *  current-shape rows (`SessionTurn.type` is the published
 *  `SessionEventType` union), so every tag is spelled literally
 *  and none needs a widening cast. */
function fold(rows: ReadonlyArray<SessionTurn>, sessionId = 'sess-1') {
  return projectRows(newRowProjection(sessionId), rows);
}

/** The provider `Message` a row carries — the wire type, complete.
 *  A fixture that omits `tool_call_id`, `name` or
 *  `tool_result_cleared_at` would be spelling a payload the contract
 *  does not publish, so every message row is built through this one
 *  shape. */
function providerMessage(
  role: Message['role'],
  content: Content,
  extra: Partial<Omit<Message, 'role' | 'content'>> = {},
): Message {
  const message: Message = {
    role,
    content,
    tool_call_id: null,
    name: null,
    tool_result_cleared_at: null,
  };
  return { ...message, ...extra };
}

describe('isToolEchoText', () => {
  it('treats prose as text, not as an echo', () => {
    expect(isToolEchoText('Here is the answer.')).toBe(false);
  });

  it('treats a tool_use payload as an echo', () => {
    expect(isToolEchoText('{"id":"call_1","name":"read","input":{"path":"a.txt"}}')).toBe(true);
  });

  it('treats a tool_result payload as an echo', () => {
    expect(isToolEchoText('{"content":"ok","tool_use_id":"call_1","is_error":false}')).toBe(true);
  });

  it('strips one SSE `data:` prefix before parsing', () => {
    expect(isToolEchoText('data: {"id":"call_1","input":{}}')).toBe(true);
  });

  it('rejects prose that merely mentions the echo keys', () => {
    expect(isToolEchoText('the payload has "id" and "input" keys')).toBe(false);
  });

  it('rejects a JSON array and a JSON primitive', () => {
    expect(isToolEchoText('[{"id":"call_1","input":{}}]')).toBe(false);
    expect(isToolEchoText('42')).toBe(false);
    expect(isToolEchoText('null')).toBe(false);
  });

  it('rejects a JSON object that is neither a tool_use nor a tool_result', () => {
    expect(isToolEchoText('{"summary":"done"}')).toBe(false);
  });
});

describe('carriesModelContent', () => {
  it('is false for a user bubble', () => {
    expect(carriesModelContent(userMessage('hello'))).toBe(false);
  });

  it('is true for an assistant turn with text', () => {
    expect(carriesModelContent(assistantMessage('hello'))).toBe(true);
  });

  it('is true for an assistant turn that only carries a tool block', () => {
    expect(
      carriesModelContent({
        id: 'assistant-1',
        role: 'assistant',
        segments: [{ id: 'seg-1', type: 'tool_block', content: '' }],
      }),
    ).toBe(true);
  });

  it('is false for an assistant-shaped bubble that carries no model content', () => {
    expect(
      carriesModelContent({
        id: 'assistant-1',
        role: 'assistant',
        segments: [{ id: 'seg-1', type: 'notice', content: '插件事件' }],
      }),
    ).toBe(false);
    expect(
      carriesModelContent({
        id: 'assistant-1',
        role: 'assistant',
        segments: [{ id: 'seg-1', type: 'compaction', content: '摘要' }],
      }),
    ).toBe(false);
  });
});

describe('messageParts', () => {
  it('returns the one part of a Single content', () => {
    expect(messageParts(providerMessage('user', { Single: { type: 'text', text: 'hi' } }))).toEqual(
      [{ type: 'text', text: 'hi' }],
    );
  });

  it('returns every part of a Multi content', () => {
    expect(
      messageParts(
        providerMessage('assistant', {
          Multi: [
            { type: 'text', text: 'a' },
            { type: 'text', text: 'b' },
          ],
        }),
      ),
    ).toEqual([
      { type: 'text', text: 'a' },
      { type: 'text', text: 'b' },
    ]);
  });

  it('returns an empty list for an empty Multi content', () => {
    expect(messageParts(providerMessage('user', { Multi: [] }))).toEqual([]);
  });
});

describe('newRowProjection', () => {
  it('starts an empty projection for one session', () => {
    expect(newRowProjection('sess-1')).toEqual({ sessionId: 'sess-1', messages: [], sources: [] });
  });
});

describe('projectRows', () => {
  it('reads a published prompt row and an assistant row', () => {
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Single: { type: 'text', text: 'hello' } }),
      },
      {
        type: 'assistant_message',
        seq: 2,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'hi there' } }),
      },
    ]);

    expect(outline(state.messages)).toEqual([
      { role: 'user', segments: [{ type: 'text', content: 'hello' }] },
      { role: 'assistant', segments: [{ type: 'text', content: 'hi there' }] },
    ]);
    expect(state.sources).toEqual([1, 2]);
  });

  it('binds an assistant turn to the session and leaves the prompt unbound', () => {
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Single: { type: 'text', text: 'hello' } }),
      },
      {
        type: 'assistant_message',
        seq: 2,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'hi there' } }),
      },
    ]);

    expect(state.messages[0].sessionId).toBeUndefined();
    expect(state.messages[1].sessionId).toBe('sess-1');
  });

  it('reads the typed dialect: an assistant turn and its tool outcome', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 7,
        data: providerMessage('assistant', {
          Multi: [
            { type: 'text', text: 'reading the file' },
            { type: 'tool_use', id: 'call_1', name: 'read', input: { path: 'a.txt' } },
          ],
        }),
      },
      {
        type: 'tool_result',
        seq: 8,
        data: providerMessage(
          'tool',
          { Multi: [{ type: 'text', text: 'file body' }] },
          { tool_call_id: 'call_1', name: 'read' },
        ),
      },
    ]);

    expect(outline(state.messages)).toEqual([
      {
        role: 'assistant',
        segments: [
          { type: 'text', content: 'reading the file' },
          {
            type: 'tool_block',
            content: '',
            toolName: 'read',
            callContent: '{\n  "path": "a.txt"\n}',
            resultContent: 'file body',
            toolPending: false,
            toolError: false,
            toolUseId: 'call_1',
          },
        ],
      },
    ]);
    // The tool outcome landed on the turn's own block, so the turn
    // still has one durable ordinal.
    expect(state.sources).toEqual([7]);
  });

  it('renders a warning row as a one-line transcript notice', () => {
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Single: { type: 'text', text: 'hello' } }),
      },
      {
        type: 'warning',
        seq: 2,
        data: {
          kind: 'strict_fold_refused',
          message: 'the session log does not fold strictly',
        },
      },
    ]);

    expect(outline(state.messages)).toEqual([
      { role: 'user', segments: [{ type: 'text', content: 'hello' }] },
      {
        role: 'assistant',
        segments: [{ type: 'warning', content: 'the session log does not fold strictly' }],
      },
    ]);
    expect(state.sources).toEqual([1, 2]);
  });

  it('labels a warning row by its kind slug when it carries no message', () => {
    const state = fold([{ type: 'warning', seq: 3, data: { kind: 'config_drift' } }]);

    expect(outline(state.messages)).toEqual([
      {
        role: 'assistant',
        segments: [{ type: 'warning', content: '会话告警（config_drift）' }],
      },
    ]);
  });

  it('renders a hook_block row as a policy-block notice', () => {
    const state = fold([
      {
        type: 'hook_block',
        seq: 4,
        data: {
          hook: 'echo-blocker',
          reason: 'echo is not allowed here',
          tool: 'echo',
          call_id: 'c1',
        },
      },
    ]);

    expect(outline(state.messages)).toEqual([
      {
        role: 'assistant',
        segments: [
          {
            type: 'hook_block',
            content: '⛔ 策略拦截 · echo-blocker 拦截了 echo — echo is not allowed here',
          },
        ],
      },
    ]);
  });

  it('names the policy but not the call when the row carries no tool', () => {
    const state = fold([
      {
        type: 'hook_block',
        seq: 5,
        data: { hook: 'echo-blocker', reason: 'echo is not allowed here' },
      },
    ]);

    expect(outline(state.messages)).toEqual([
      {
        role: 'assistant',
        segments: [
          {
            type: 'hook_block',
            content: '⛔ 策略拦截 · echo-blocker — echo is not allowed here',
          },
        ],
      },
    ]);
  });

  it('keeps a vetoed call and its error result in one bubble — the notice joins, not fences', () => {
    // The harness writes the veto in this order: the assistant row
    // that proposed the call, the hook_block row, then the synthetic
    // error result. A notice that opened its own message would fence
    // the pair, leaving the tool block pending forever and the error
    // orphaned in a bubble of its own.
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', {
          Multi: [
            { type: 'tool_use', id: 'call_1', name: 'shell', input: { command: 'rm -rf /' } },
          ],
        }),
      },
      {
        type: 'hook_block',
        seq: 2,
        data: {
          hook: 'shell-deny',
          reason: 'rm -rf is not allowed',
          tool: 'shell',
          call_id: 'call_1',
        },
      },
      {
        type: 'tool_result',
        seq: 3,
        is_error: true,
        data: providerMessage(
          'tool',
          {
            Multi: [
              {
                type: 'text',
                text: '[blocked by guard `shell-deny` (high)] rm -rf is not allowed',
              },
            ],
          },
          { tool_call_id: 'call_1', name: 'shell' },
        ),
      },
    ]);

    expect(state.messages).toHaveLength(1);
    const block = state.messages[0].segments.find((segment) => segment.type === 'tool_block');
    expect(block?.toolPending).toBe(false);
    expect(block?.toolError).toBe(true);
    expect(block?.resultContent).toContain('blocked by guard `shell-deny`');
    const notice = state.messages[0].segments.find((segment) => segment.type === 'hook_block');
    expect(notice?.content).toBe('⛔ 策略拦截 · shell-deny 拦截了 shell — rm -rf is not allowed');
  });

  it('renders the policy-block notice identically live and after a reload', () => {
    // The row the veto writes is the one fact; whether it reaches the
    // page as a live row frame or in a reloaded baseline, the notice
    // is the same glyph and the same wording.
    const expected = '⛔ 策略拦截 · shell-deny 拦截了 shell — rm -rf is not allowed';
    const prompt: SessionTurn = {
      type: 'user_message',
      seq: 1,
      data: providerMessage('user', { Multi: [{ type: 'text', text: 'run it' }] }),
    };
    const row: SessionTurn = {
      type: 'hook_block',
      seq: 4,
      data: {
        hook: 'shell-deny',
        reason: 'rm -rf is not allowed',
        tool: 'shell',
        call_id: 'call_1',
      },
    };

    const opened = projectRows(newRowProjection('sess-1'), [prompt]);
    const live = projectRows(opened, [row]);
    const reloaded = projectRows(newRowProjection('sess-1'), [prompt, row]);

    expect(
      live.messages
        .find((m) => m.segments.some((s) => s.type === 'hook_block'))
        ?.segments.find((s) => s.type === 'hook_block')?.content,
    ).toBe(expected);
    expect(
      reloaded.messages
        .find((m) => m.segments.some((s) => s.type === 'hook_block'))
        ?.segments.find((s) => s.type === 'hook_block')?.content,
    ).toBe(expected);
  });

  it('pairs a tool outcome with the block that named the same call', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', {
          Multi: [
            { type: 'tool_use', id: 'call_1', name: 'read', input: { path: 'a' } },
            { type: 'tool_use', id: 'call_2', name: 'grep', input: { pattern: 'x' } },
          ],
        }),
      },
      {
        type: 'tool_result',
        seq: 2,
        data: providerMessage(
          'tool',
          { Multi: [{ type: 'text', text: 'file body' }] },
          { tool_call_id: 'call_1', name: 'read' },
        ),
      },
    ]);

    // The outcome names the FIRST of the two open calls, so it can only
    // land on that block — the trailing pending block is another call.
    const blocks = state.messages[0].segments.filter((segment) => segment.type === 'tool_block');
    expect(blocks).toHaveLength(2);
    expect(blocks[0].toolPending).toBe(false);
    expect(blocks[0].resultContent).toBe('file body');
    expect(blocks[1].toolPending).toBe(true);
    expect(blocks[1].resultContent).toBeUndefined();
  });

  it('reads the crash-repair row dialect: a call id, an error code, an interrupted flag', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', {
          Multi: [{ type: 'tool_use', id: 'call_1', name: 'read', input: {} }],
        }),
      },
      {
        type: 'tool_result',
        seq: 2,
        data: { call_id: 'call_1', tool_name: 'read', error: 'timeout', interrupted: true },
      },
    ]);

    const block = state.messages[0].segments[0];
    expect(block.resultContent).toBe('timeout');
    expect(block.toolError).toBe(true);
    expect(block.toolPending).toBe(false);
  });

  it('marks a failed tool outcome as failed', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', {
          Multi: [{ type: 'tool_use', id: 'call_1', name: 'read', input: {} }],
        }),
      },
      {
        type: 'tool_result',
        seq: 2,
        is_error: true,
        data: providerMessage(
          'tool',
          { Multi: [{ type: 'text', text: 'no such file' }] },
          { tool_call_id: 'call_1', name: 'read' },
        ),
      },
    ]);

    expect(state.messages[0].segments[0].toolError).toBe(true);
    expect(state.messages[0].segments[0].resultContent).toBe('no such file');
  });

  it('coalesces the text of one assistant turn into one segment', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'first ' } }),
      },
      {
        type: 'assistant_message',
        seq: 2,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'second' } }),
      },
    ]);

    expect(outline(state.messages)).toEqual([
      { role: 'assistant', segments: [{ type: 'text', content: 'first second' }] },
    ]);
    expect(state.sources).toEqual([1]);
  });

  it('starts a new assistant turn after every prompt', () => {
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Single: { type: 'text', text: 'q1' } }),
      },
      {
        type: 'assistant_message',
        seq: 2,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'a1' } }),
      },
      {
        type: 'user_message',
        seq: 3,
        data: providerMessage('user', { Single: { type: 'text', text: 'q2' } }),
      },
      {
        type: 'assistant_message',
        seq: 4,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'a2' } }),
      },
    ]);

    expect(state.messages.map((message) => message.role)).toEqual([
      'user',
      'assistant',
      'user',
      'assistant',
    ]);
    expect(outline(state.messages).map((message) => message.segments[0].content)).toEqual([
      'q1',
      'a1',
      'q2',
      'a2',
    ]);
  });

  it('drops the span a regenerate replaces', () => {
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Single: { type: 'text', text: 'first prompt' } }),
      },
      {
        type: 'assistant_message',
        seq: 2,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'first answer' } }),
      },
      {
        type: 'user_message',
        seq: 3,
        data: providerMessage('user', { Single: { type: 'text', text: 'second prompt' } }),
        surface_op: { start: 0, end: 2, source_event_seqs: [1, 2] },
      },
    ]);

    expect(outline(state.messages)).toEqual([
      { role: 'user', segments: [{ type: 'text', content: 'second prompt' }] },
    ]);
    expect(state.sources).toEqual([3]);
  });

  it('reads a compaction row as a marker, not as a replace', () => {
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Single: { type: 'text', text: 'q' } }),
      },
      {
        type: 'assistant_message',
        seq: 2,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'a' } }),
      },
      {
        type: 'compaction',
        seq: 3,
        data: { summary: '摘要' },
        surface_op: { start: 0, end: 2, source_event_seqs: [1, 2] },
      },
    ]);

    expect(state.messages).toHaveLength(3);
    const marker = state.messages[2];
    expect(marker.role).toBe('assistant');
    expect(marker.segments[0].type).toBe('compaction');
    expect(marker.segments[0].content).toBe('摘要');
    expect(marker.segments[0].compaction).toEqual({
      summary: '摘要',
      start: 0,
      end: 2,
      shadowedMessages: 2,
      firstSeq: 1,
      lastSeq: 2,
    });
  });

  it("renders a published user turn with its attachment refs at the row's top level", () => {
    // The writer every turn now uses: the text is in the provider
    // message's content parts, and the attachment refs ride the row's
    // own `attachments` field. One bubble, one chip.
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Multi: [{ type: 'text', text: 'see attached' }] }),
        attachments: [{ kind: 'file', name: 'notes.md', mime_type: 'text/markdown', byte_len: 12 }],
      },
    ]);

    expect(state.messages).toHaveLength(1);
    expect(state.messages[0].role).toBe('user');
    expect(state.messages[0].segments).toHaveLength(1);
    expect(state.messages[0].segments[0].type).toBe('text');
    expect(state.messages[0].segments[0].content).toBe('see attached');
    expect(state.messages[0].segments[0].files).toEqual([
      {
        id: expect.any(String),
        filename: 'notes.md',
        mimeType: 'text/markdown',
        byteLen: 12,
      },
    ]);
    expect(state.sources).toEqual([1]);
  });

  it('shows a typed file-only turn as a bubble with a chip', () => {
    // A file-only send has no text part: the bubble must still land, or
    // the turn reads as if the user sent nothing.
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Multi: [] }),
        attachments: [{ kind: 'file', name: 'notes.md', mime_type: 'text/markdown', byte_len: 12 }],
      },
    ]);

    expect(state.messages).toHaveLength(1);
    expect(state.messages[0].role).toBe('user');
    expect(state.messages[0].segments[0].content).toBe('');
    expect(state.messages[0].segments[0].files?.map((file) => file.filename)).toEqual(['notes.md']);
  });

  it('treats the append surface_op as no replace, so nothing is shadowed', () => {
    // A steer or an ordinary typed row carries `surface_op: 'append'`
    // (the string form). It shadows nothing — the replace range only
    // exists on the object form — and the compaction marker that also
    // carries it reports no span.
    const state = fold([
      {
        type: 'user_message',
        seq: 1,
        data: providerMessage('user', { Multi: [{ type: 'text', text: 'q1' }] }),
        surface_op: 'append',
      },
      {
        type: 'assistant_message',
        seq: 2,
        data: providerMessage('assistant', { Multi: [{ type: 'text', text: 'a1' }] }),
      },
      {
        type: 'user_message',
        seq: 3,
        data: providerMessage('user', { Multi: [{ type: 'text', text: 'q2' }] }),
        surface_op: 'append',
      },
      {
        type: 'compaction',
        seq: 4,
        data: { summary: '摘要' },
        surface_op: 'append',
      },
    ]);

    expect(state.messages.map((message) => message.role)).toEqual([
      'user',
      'assistant',
      'user',
      'assistant',
    ]);
    expect(outline(state.messages).map((message) => message.segments[0].content)).toEqual([
      'q1',
      'a1',
      'q2',
      '摘要',
    ]);
    expect(state.messages[3].segments[0].compaction).toEqual({ summary: '摘要' });
    expect(state.sources).toEqual([1, 2, 3, 4]);
  });

  it('tracks the durable ordinal of the row each message came from', () => {
    const state = fold([
      {
        type: 'user_message',
        data: providerMessage('user', { Single: { type: 'text', text: 'a' } }),
      },
      {
        type: 'assistant_message',
        seq: 7,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'b' } }),
      },
      {
        type: 'user_message',
        data: providerMessage('user', { Single: { type: 'text', text: 'c' } }),
      },
    ]);

    expect(state.sources).toEqual([1, 7, 8]);
  });

  it('leaves the projection it was handed untouched', () => {
    const before = newRowProjection('sess-1');
    const after = projectRows(before, [
      {
        type: 'user_message',
        data: providerMessage('user', { Single: { type: 'text', text: 'x' } }),
      },
    ]);

    expect(before.messages).toEqual([]);
    expect(before.sources).toEqual([]);
    expect(after).not.toBe(before);
    expect(after.messages).toHaveLength(1);
  });

  it('names the assistant row a published turn came from', () => {
    // The published row's seq is the answer's identity: it is what a
    // rating names once the turn is only a reconstructed row.
    const state = fold([
      {
        type: 'user_message',
        seq: 6,
        data: providerMessage('user', { Single: { type: 'text', text: 'hello' } }),
      },
      {
        type: 'assistant_message',
        seq: 7,
        data: providerMessage('assistant', { Single: { type: 'text', text: 'hi there' } }),
      },
    ]);

    expect(feedbackTarget(state.messages[1])).toBe('7');
  });

  it('renders nothing for a row that carries no type', () => {
    const state = fold([
      { data: providerMessage('user', { Single: { type: 'text', text: 'hello' } }) },
    ]);

    expect(state.messages).toEqual([]);
    expect(state.sources).toEqual([]);
  });

  it('reads a published tool outcome as a tool_result row', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', {
          Single: { type: 'tool_use', id: 'call_1', name: 'read', input: { path: 'a.txt' } },
        }),
      },
      {
        type: 'tool_result',
        seq: 2,
        data: providerMessage(
          'tool',
          {
            Single: {
              type: 'tool_result',
              tool_use_id: 'call_1',
              content: [{ type: 'text', text: 'file body' }],
              structured_content: null,
              is_error: false,
            },
          },
          { tool_call_id: 'call_1', name: 'read' },
        ),
      },
    ]);

    expect(outline(state.messages)).toEqual([
      {
        role: 'assistant',
        segments: [
          {
            type: 'tool_block',
            content: '',
            toolName: 'read',
            callContent: '{\n  "path": "a.txt"\n}',
            resultContent: 'file body',
            toolPending: false,
            toolError: false,
            toolUseId: 'call_1',
          },
        ],
      },
    ]);
    expect(state.sources).toEqual([1]);
  });
});

describe('reconstructMessagesFromSession', () => {
  it('reconstructs from the session log rows when the log has rows', () => {
    const session: SessionDetail = {
      id: 'sess-1',
      status: 'completed',
      context_id: 'ctx-1',
      history: [
        {
          type: 'user_message',
          seq: 1,
          data: providerMessage('user', { Single: { type: 'text', text: 'hello' } }),
        },
        {
          type: 'assistant_message',
          seq: 2,
          data: providerMessage('assistant', { Multi: [{ type: 'text', text: 'hi there' }] }),
        },
      ],
      artifacts: [],
    };

    expect(outline(reconstructMessagesFromSession(session))).toEqual([
      { role: 'user', segments: [{ type: 'text', content: 'hello' }] },
      { role: 'assistant', segments: [{ type: 'text', content: 'hi there' }] },
    ]);
  });

  it('falls back to the legacy artifact dialect when the log has no rows', () => {
    const session: SessionDetail = {
      id: 'sess-2',
      status: 'completed',
      context_id: 'ctx-2',
      history: [],
      artifacts: [
        {
          metadata: { kind: 'tool_call', tool_use_id: 'call_9', tool_name: 'grep' },
          parts: [{ text: 'grep -r foo' }],
        },
        {
          metadata: { kind: 'tool_result', tool_use_id: 'call_9' },
          parts: [{ text: '3 matches' }],
        },
      ],
    };

    expect(outline(reconstructMessagesFromSession(session))).toEqual([
      {
        role: 'assistant',
        segments: [
          {
            type: 'tool_block',
            content: '',
            toolName: 'grep',
            callContent: 'grep -r foo',
            resultContent: '3 matches',
            toolPending: false,
          },
        ],
      },
    ]);
  });

  it('reconstructs nothing from an empty session', () => {
    const session: SessionDetail = {
      id: 'sess-3',
      status: 'completed',
      context_id: 'ctx-3',
      history: [],
      artifacts: [],
    };

    expect(reconstructMessagesFromSession(session)).toEqual([]);
  });
});

describe('mergeReconstructedMessages', () => {
  it('leaves the store alone when the same prompt is already there', () => {
    const existing = [userMessage('hello')];
    const reconstructed = [userMessage('hello'), assistantMessage('hi there')];

    const merged = mergeReconstructedMessages(existing, reconstructed);

    expect(merged).toHaveLength(1);
    expect(merged[0]).toBe(existing[0]);
  });

  it('drops the reconstructed turns whose session is already in the store', () => {
    const existing = [assistantMessage('the answer', 'sess-1')];
    const reconstructed = [assistantMessage('the answer', 'sess-1'), userMessage('new prompt')];

    const merged = mergeReconstructedMessages(existing, reconstructed);

    expect(merged).toHaveLength(2);
    expect(merged[0]).toBe(existing[0]);
    expect(merged[1].role).toBe('user');
  });

  it('appends the reconstructed prompts that carry no session id', () => {
    const existing = [assistantMessage('the answer', 'sess-1')];
    const reconstructed = [userMessage('new prompt')];

    const merged = mergeReconstructedMessages(existing, reconstructed);

    expect(merged.map((message) => message.role)).toEqual(['assistant', 'user']);
  });

  it('returns a copy of the store when there is nothing to merge', () => {
    const existing = [userMessage('hello')];

    const merged = mergeReconstructedMessages(existing, []);

    expect(merged).toHaveLength(1);
    expect(merged[0]).toBe(existing[0]);
    expect(merged).not.toBe(existing);
  });
});

describe('firstUserPromptText', () => {
  it('returns the text of the first user prompt', () => {
    expect(
      firstUserPromptText([
        assistantMessage('an answer'),
        userMessage('ask'),
        userMessage('ask again'),
      ]),
    ).toBe('ask');
  });

  it('skips a user prompt that carries no text', () => {
    expect(firstUserPromptText([userMessage(''), userMessage('later')])).toBe('later');
  });

  it('returns null when no user message carries text', () => {
    expect(firstUserPromptText([assistantMessage('an answer')])).toBeNull();
    expect(firstUserPromptText([])).toBeNull();
  });
});

describe('selectLastAssistant', () => {
  /** A timeline annotation the projection renders in-line. */
  function annotationMessage(
    type: 'compaction' | 'warning' | 'notice' | 'hook_block',
  ): ChatMessageLike {
    return {
      id: 'annotation-1',
      role: 'assistant',
      segments: [{ id: 'seg-1', type, content: 'a timeline fact' }],
    };
  }

  it('skips a trailing warning notice: a failed turn leaves the notice as the last assistant row', () => {
    const answer = assistantMessage('an answer');
    const selected = selectLastAssistant([
      userMessage('ask'),
      answer,
      annotationMessage('warning'),
    ]);
    expect(selected).toBe(answer);
  });

  it('skips a policy-block notice: a blocked call is not an answer to rate', () => {
    expect(selectLastAssistant([annotationMessage('hook_block')])).toBeNull();
    const answer = assistantMessage('an answer');
    expect(selectLastAssistant([answer, annotationMessage('hook_block')])).toBe(answer);
  });

  it('skips compaction markers and subagent notices the same way', () => {
    expect(selectLastAssistant([annotationMessage('compaction')])).toBeNull();
    const answer = assistantMessage('an answer');
    expect(selectLastAssistant([answer, annotationMessage('notice')])).toBe(answer);
  });

  it('returns null when the transcript has no assistant answer at all', () => {
    expect(selectLastAssistant([userMessage('ask'), annotationMessage('warning')])).toBeNull();
    expect(selectLastAssistant([])).toBeNull();
  });
});

describe('tool_output rows replay the streamed progress', () => {
  /** One streamed chunk of a running call, as the writer logs it. */
  function chunk(seq: number, output: string, callId = 'call_1'): SessionTurn {
    return {
      type: 'tool_output',
      seq,
      call_id: callId,
      data: { output },
    };
  }

  it('appends each chunk to the pending block and leaves it pending', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', {
          Multi: [{ type: 'tool_use', id: 'call_1', name: 'shell', input: { command: 'seq 1' } }],
        }),
      },
      chunk(2, 'first '),
      chunk(3, 'second'),
    ]);

    expect(outline(state.messages)).toEqual([
      {
        role: 'assistant',
        segments: [
          {
            type: 'tool_block',
            content: '',
            toolName: 'shell',
            callContent: '{\n  "command": "seq 1"\n}',
            resultContent: 'first second',
            toolPending: true,
            toolUseId: 'call_1',
          },
        ],
      },
    ]);
  });

  it('lets the settled tool_result replace the preview', () => {
    const state = fold([
      {
        type: 'assistant_message',
        seq: 1,
        data: providerMessage('assistant', {
          Multi: [{ type: 'tool_use', id: 'call_1', name: 'shell', input: { command: 'seq 1' } }],
        }),
      },
      chunk(2, 'first '),
      chunk(3, 'second'),
      {
        type: 'tool_result',
        seq: 4,
        data: providerMessage(
          'tool',
          { Multi: [{ type: 'text', text: 'first second\n\n[exit code: 0]' }] },
          { tool_call_id: 'call_1', name: 'shell' },
        ),
      },
    ]);

    expect(outline(state.messages)).toEqual([
      {
        role: 'assistant',
        segments: [
          {
            type: 'tool_block',
            content: '',
            toolName: 'shell',
            callContent: '{\n  "command": "seq 1"\n}',
            resultContent: 'first second\n\n[exit code: 0]',
            toolPending: false,
            toolError: false,
            toolUseId: 'call_1',
          },
        ],
      },
    ]);
  });

  it('drops a chunk that names no pending block', () => {
    const state = fold([chunk(1, 'orphan', 'call_missing')]);
    expect(state.messages).toEqual([]);
  });
});
