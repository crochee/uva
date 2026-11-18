import { describe, expect, it } from 'vitest';

import { liveFrameAdmitted, retiresProjection } from '../src/lib/live-projection';

/**
 * The projection's whole retirement rule, in one place.
 *
 * The projection is one step's live window, so the comparison is
 * against the log position where that step began — not against an id
 * and not against the text. A turn boundary retires it too, once the
 * run is over: nothing else will replace the projection.
 */
describe('retiresProjection', () => {
  it('retires the projection on the assistant row past its start', () => {
    expect(
      retiresProjection({ type: 'assistant_message' }, 13, { startSeq: 12, running: true }),
    ).toBe(true);
  });

  it('retires the projection on the tool result past its start', () => {
    expect(retiresProjection({ type: 'tool_result' }, 13, { startSeq: 12, running: true })).toBe(
      true,
    );
  });

  it('keeps the projection when the row is older than its step', () => {
    expect(
      retiresProjection({ type: 'assistant_message' }, 11, { startSeq: 12, running: true }),
    ).toBe(false);
  });

  it('keeps the projection for a row that is not content', () => {
    // The prompt, the step boundary and the usage frame are not what
    // the projection renders: retiring it on those would blank the
    // streaming answer.
    expect(retiresProjection({ type: 'user_message' }, 13, { startSeq: 12, running: true })).toBe(
      false,
    );
    expect(retiresProjection({ type: 'usage' }, 13, { startSeq: 12, running: true })).toBe(false);
    expect(retiresProjection({ type: 'step' }, 13, { startSeq: 12, running: true })).toBe(false);
  });

  it('keeps the projection when the row sits exactly at its start', () => {
    // The step's own boundary row is the position the projection
    // started at, not past it.
    expect(
      retiresProjection({ type: 'assistant_message' }, 12, { startSeq: 12, running: true }),
    ).toBe(false);
  });

  it('retires the projection on a turn boundary once the run is over', () => {
    const boundary = { type: 'turn', data: { kind: 'end' } } as const;
    expect(retiresProjection(boundary, 13, { startSeq: 12, running: false })).toBe(true);
  });

  it('retires the projection on a turn start once the run is over', () => {
    const boundary = { type: 'turn', data: { kind: 'start' } } as const;
    expect(retiresProjection(boundary, 13, { startSeq: 12, running: false })).toBe(true);
  });

  it('keeps the projection on a turn boundary while the run is running', () => {
    const boundary = { type: 'turn', data: { kind: 'end' } } as const;
    expect(retiresProjection(boundary, 13, { startSeq: 12, running: true })).toBe(false);
  });
});

/**
 * The come-back guard: a frame for a projection the durable rows
 * already retired must not bring it back.
 */
describe('liveFrameAdmitted', () => {
  it('admits any frame while nothing was retired', () => {
    expect(liveFrameAdmitted(12, null)).toBe(true);
  });

  it('rejects a frame at or before the retired position', () => {
    expect(liveFrameAdmitted(12, 12)).toBe(false);
    expect(liveFrameAdmitted(11, 12)).toBe(false);
  });

  it('admits a later step', () => {
    expect(liveFrameAdmitted(13, 12)).toBe(true);
  });
});
