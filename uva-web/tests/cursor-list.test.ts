/**
 * The pure core of `useCursorList`.
 *
 * Seam: `src/lib/cursor-list.ts` holds the rules that decide what a
 * cursor-paginated list shows — how one response is folded into the
 * accumulated items, when `loadMore()` may fire, and the paging
 * state the guard reads. It catches a response being folded in
 * wrongly (items duplicated, lost or reordered), a second page being
 * asked for while one is in flight or after the list ran out of
 * pages, and a request claim that does not survive to the guard. It
 * misses the hook's own wiring: whether `fetchPage` actually claims
 * and settles this state — that is `./use-cursor-list.test.ts`.
 *
 * The expected values are hand-written examples, not recomputed
 * from the module under test.
 */
import { describe, expect, it } from 'vitest';

import {
  beginCursorRequest,
  mergeCursorPage,
  nextCursorToLoad,
  settleCursorLoad,
} from '../src/lib/cursor-list';

describe('mergeCursorPage', () => {
  it('puts the next page after the items already loaded', () => {
    const loaded = [{ id: 'a' }, { id: 'b' }];
    const page = [{ id: 'c' }, { id: 'd' }];

    expect(mergeCursorPage(loaded, page, 'append')).toEqual([
      { id: 'a' },
      { id: 'b' },
      { id: 'c' },
      { id: 'd' },
    ]);
  });

  it('drops the items already loaded when the response replaces the list', () => {
    expect(mergeCursorPage([{ id: 'a' }, { id: 'b' }], [{ id: 'z' }], 'replace')).toEqual([
      { id: 'z' },
    ]);
  });

  it('keeps the items already loaded when the next page is empty', () => {
    expect(mergeCursorPage([{ id: 'a' }, { id: 'b' }], [], 'append')).toEqual([
      { id: 'a' },
      { id: 'b' },
    ]);
  });

  it('leaves the items already loaded untouched', () => {
    const loaded = [{ id: 'a' }];

    mergeCursorPage(loaded, [{ id: 'b' }], 'append');

    expect(loaded).toEqual([{ id: 'a' }]);
  });
});

describe('nextCursorToLoad', () => {
  it('asks for the next cursor when no request is in flight', () => {
    expect(nextCursorToLoad(false, 'cursor-2')).toBe('cursor-2');
  });

  it('asks for nothing while a request is in flight', () => {
    expect(nextCursorToLoad(true, 'cursor-2')).toBeNull();
  });

  it('asks for nothing when the list has no next page', () => {
    expect(nextCursorToLoad(false, null)).toBeNull();
  });

  it('asks for nothing when the cursor is empty', () => {
    expect(nextCursorToLoad(false, '')).toBeNull();
  });
});

describe('cursor paging state', () => {
  it('claims the request so a second load in the same tick is refused', () => {
    const loading = beginCursorRequest({ nextCursor: 'c2', inFlight: false });

    expect(nextCursorToLoad(loading.inFlight, loading.nextCursor)).toBeNull();
  });

  it('keeps the next cursor when a request starts', () => {
    expect(beginCursorRequest({ nextCursor: 'c2', inFlight: false })).toEqual({
      nextCursor: 'c2',
      inFlight: true,
    });
  });

  it('settles with the cursor of the page after the response', () => {
    expect(settleCursorLoad('c3')).toEqual({ nextCursor: 'c3', inFlight: false });
  });

  it('settles with no next cursor when the list ran out of pages', () => {
    expect(settleCursorLoad(null)).toEqual({ nextCursor: null, inFlight: false });
  });
});
