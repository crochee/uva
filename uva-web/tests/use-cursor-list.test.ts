/**
 * The `loadMore()` race in `useCursorList`.
 *
 * Two `loadMore()` calls in the same tick used to both pass the
 * in-flight guard: it read the paging state back out of refs that are
 * refreshed only when React re-renders, so every call in the gap
 * between a fetch starting (or landing) and that re-render saw the
 * state of one render ago. A second call in the gap therefore issued
 * the page request a second time, and — when it came after a response
 * had landed but before the re-render — asked for the page just
 * loaded again and appended it twice.
 *
 * Seam: the real `useCursorList` file, driven through the hook
 * runtime in `./react-hook-runtime.ts` with the typed page fetcher the
 * caller would pass (`FetchCursorPage`) replaced by a scriptable fake.
 * It catches what the hook itself decides: which cursor it asks for,
 * and which items it accumulates. It misses everything the runtime
 * does not model — React's batching windows, layout effects, and the
 * DOM the pages render into (AGENTS.md §4.2). Both timings were
 * cross-checked against real React before the fix, in a throwaway
 * probe: it issued the same request sequences, and its second timing
 * the same duplicated page.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { createHookRuntime, type HookRuntime } from './react-hook-runtime';
import {
  useCursorList,
  type FetchCursorPage,
  type UseCursorListResult,
} from '../src/hooks/useCursorList';
import type { List } from '../src/api/types';

interface Item {
  id: string;
}

/** One not-yet-answered page request. */
interface Pending {
  cursor: string | null;
  promise: Promise<List<Item>>;
  settle: (page: List<Item>) => void;
}

const requests: Pending[] = [];
/** Every cursor asked for, in order — the record the assertions read. */
const issued: Array<string | null> = [];
/** The page each cursor answers with, keyed like the server's pages. */
const pages: Record<string, List<Item>> = {
  '': { data: [{ id: 'a1' }], next_cursor: 'c2', total: 3 },
  c2: { data: [{ id: 'a2' }], next_cursor: 'c3', total: 3 },
  c3: { data: [{ id: 'a3' }], next_cursor: null, total: 3 },
};

// The page is answered by the test, not the clock, so the promise has
// to be deferred here. `Promise.withResolvers()` would be the neater
// spelling, but CI's Node 20 predates it.
const fetchPage: FetchCursorPage<Item> = (cursor) => {
  let settle!: (page: List<Item>) => void;
  const promise = new Promise<List<Item>>((resolve) => {
    settle = resolve;
  });
  issued.push(cursor);
  requests.push({ cursor, promise, settle });
  return promise;
};

// The hook imports React's four primitives; these wrappers stand in
// for them, late-bound to the per-test `runtime` below (the mock
// factory runs before the module body assigns it).
let runtime: HookRuntime;
vi.mock('react', () => ({
  useCallback: (fn: unknown, deps: readonly unknown[]) => runtime.useCallback(fn, deps),
  useEffect: (fn: () => void, deps?: readonly unknown[]) => runtime.useEffect(fn, deps),
  useRef: (initial: unknown) => runtime.useRef(initial),
  useState: (initial: unknown) => runtime.useState(initial),
}));

let latest: UseCursorListResult<Item>;

function Probe() {
  latest = useCursorList<Item>(fetchPage, 'items');
  return null;
}

/**
 * Answer every outstanding request with its cursor's page, then wait
 * for the hook's own continuation — the request promises it is
 * awaiting — so no clock and no guessed delay is involved. Aborted
 * requests settle too; the hook swallows their rejection.
 */
const answerRequests = async () => {
  const pending = requests.splice(0);
  for (const request of pending) request.settle(pages[request.cursor ?? '']);
  await Promise.allSettled(pending.map((request) => request.promise));
};

/** Mount, load the first page, and let the re-render after it happen. */
const firstPageLoaded = async () => {
  runtime.render(Probe);
  await answerRequests();
  runtime.render(Probe);
};

describe('useCursorList loadMore', () => {
  beforeEach(() => {
    runtime = createHookRuntime();
    issued.length = 0;
    requests.length = 0;
  });

  it('issues one request for two loadMore() calls in the same tick', async () => {
    await firstPageLoaded();
    expect(latest.items.map((item) => item.id)).toEqual(['a1']);

    const first = latest.loadMore();
    // Same tick as `first`: no re-render has run between the two, so
    // a guard that reads render-refreshed state still sees "idle".
    const second = latest.loadMore();
    await answerRequests();
    await Promise.all([first, second]);
    runtime.render(Probe);

    expect(issued).toEqual([null, 'c2']);
    expect(latest.items.map((item) => item.id)).toEqual(['a1', 'a2']);
  });

  it('asks for the next cursor when loadMore() runs before the list re-renders', async () => {
    await firstPageLoaded();

    const secondPage = latest.loadMore();
    await answerRequests();
    await secondPage;
    // No re-render here: the response has landed and been folded in,
    // but any render-refreshed mirror of it is still one render old.
    const thirdPage = latest.loadMore();
    await answerRequests();
    await thirdPage;
    runtime.render(Probe);

    expect(issued).toEqual([null, 'c2', 'c3']);
    expect(latest.items.map((item) => item.id)).toEqual(['a1', 'a2', 'a3']);
  });
});
