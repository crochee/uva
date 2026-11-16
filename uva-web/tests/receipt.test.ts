/**
 * The four-state receipt of a profile transaction.
 *
 * Seam: `src/settings/receipt.ts` turns a `ProfileChange` (what the
 * server answered) into the row feedback a person reads. It catches two
 * states collapsing into one — the failure this module exists to
 * prevent. It misses the write path that produces the receipt.
 *
 * The expected strings are the contract itself: four distinct wordings,
 * each saying what actually happened to the deployment. They are not
 * recomputed from the module under test.
 */
import { describe, expect, it } from 'vitest';

import type { ProfileChange, ProfileChangeState } from '../src/api/settings-profile';
import { failureReceipt, receiptOf } from '../src/settings/receipt';

/** The four states a transaction can end in. */
const STATES: ReadonlyArray<ProfileChangeState> = [
  'applied',
  'restart-required',
  'overridden',
  'failed',
];

function change(state: ProfileChangeState, reasons: string[] = []): ProfileChange {
  return { state, reasons };
}

describe('receiptOf', () => {
  it('says an applied change was applied', () => {
    expect(receiptOf(change('applied'))).toEqual({ tone: 'applied', text: '已应用' });
  });

  it('says a change that needs a restart was written but is not live yet', () => {
    expect(receiptOf(change('restart-required'))).toEqual({
      tone: 'warn',
      text: '已写入 · 重启后生效',
    });
  });

  it('says an overridden field was not written at all', () => {
    expect(receiptOf(change('overridden'))).toEqual({ tone: 'warn', text: '未写入 · 被上层覆盖' });
  });

  it('says a failure changed nothing', () => {
    expect(receiptOf(change('failed'))).toEqual({ tone: 'error', text: '失败' });
  });

  it('names the reason the server gave', () => {
    expect(receiptOf(change('applied', ['写入 2 行']))).toEqual({
      tone: 'applied',
      text: '已应用：写入 2 行',
    });
    expect(receiptOf(change('overridden', ['上层 profile 已声明']))).toEqual({
      tone: 'warn',
      text: '未写入 · 被上层覆盖：上层 profile 已声明',
    });
  });

  it('never collapses the four states into one wording', () => {
    const texts = STATES.map((state) => receiptOf(change(state)).text);
    expect(new Set(texts).size).toBe(4);
  });

  it('never collapses the four states into one tone', () => {
    // `restart-required` and `overridden` share a tone on purpose —
    // both are warnings — so the wording is what keeps them apart.
    const tones = STATES.map((state) => receiptOf(change(state)).tone);
    expect(new Set(tones).size).toBe(3);
    expect(receiptOf(change('restart-required')).tone).toBe(receiptOf(change('overridden')).tone);
    expect(receiptOf(change('applied')).tone).not.toBe(receiptOf(change('failed')).tone);
  });
});

describe('failureReceipt', () => {
  it('reports an error with the error’s own words', () => {
    expect(failureReceipt(new Error('网络中断'))).toEqual({
      tone: 'error',
      text: '失败：网络中断',
    });
  });

  it('reports a value that is not an error the same way', () => {
    expect(failureReceipt('boom')).toEqual({ tone: 'error', text: '失败：boom' });
  });

  it('does not read as a saved change', () => {
    expect(failureReceipt(new Error('boom')).text).not.toBe(receiptOf(change('applied')).text);
    expect(failureReceipt(new Error('boom')).text).not.toBe(receiptOf(change('failed')).text);
  });
});
