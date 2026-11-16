/**
 * Cursor-based pagination hook for v1 list methods.
 *
 * Wraps the typical "fetch first page on mount, append more on
 * demand" pattern used by every list page (sessions, agents, skills).
 * The caller passes a **typed fetch function** from the domain client
 * (`sessionsApi.list`, `agentsApi.list`, …) that maps a cursor onto
 * one page; the hook never speaks URLs, so a wire rename lands in the
 * client once.
 *
 * The hook resets the accumulated items whenever `resetKey` changes
 * (the caller names the list — e.g. `'palette:sessions'`), then
 * fetches the first page. Calling `loadMore()` while a request is in
 * flight or when there is no `next_cursor` is a no-op.
 *
 * The rules behind those sentences — how a response is folded into the
 * items, when `loadMore()` may fire, and the paging state the guard
 * reads — live in `../lib/cursor-list.ts`, where they are pure and
 * testable without a renderer. The hook owns that paging state
 * synchronously: it is claimed when a request starts and settled when
 * the response lands, so two `loadMore()` calls in one tick cannot
 * both pass the guard.
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import type { List } from '../api/types';
import {
  beginCursorRequest,
  mergeCursorPage,
  nextCursorToLoad,
  settleCursorLoad,
  type CursorPaging,
} from '../lib/cursor-list';

/** One page of a list method: `null` asks for the first page. */
export type FetchCursorPage<T> = (cursor: string | null, signal: AbortSignal) => Promise<List<T>>;

export interface UseCursorListResult<T> {
  items: T[];
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  total: number | null;
  loadMore: () => Promise<void>;
  refresh: () => Promise<void>;
  /// Imperative local update so callers can apply a server-confirmed
  /// response without an extra page fetch. Use sparingly; prefer
  /// `refresh()` when the server might have mutated more than the one
  /// item you patched.
  setItems: React.Dispatch<React.SetStateAction<T[]>>;
}

export function useCursorList<T>(
  fetchPage: FetchCursorPage<T>,
  resetKey: string,
): UseCursorListResult<T> {
  const [items, setItems] = useState<T[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [total, setTotal] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Paging state the hook owns synchronously: `beginCursorRequest`
  // claims it when a request starts and `settleCursorLoad` releases
  // it when the response lands, both inside `load` rather than on a
  // re-render. The guard in `loadMore` reads it, so a second call in
  // the same tick sees the first request in flight instead of the
  // state of one render ago — and, once a page has landed, asks for
  // the page after it rather than for it again.
  const pagingRef = useRef<CursorPaging>({ nextCursor: null, inFlight: false });

  // The caller's fetch closure is a fresh function every render; the
  // ref keeps the stable request path reading the latest one without
  // restarting the effect on each render.
  const fetchPageRef = useRef(fetchPage);
  fetchPageRef.current = fetchPage;

  // Tracks the most-recent in-flight `AbortController`. Both the
  // mount-reset effect and `loadMore()` route through `fetchPage`
  // so centralizing the controller here ensures only one request
  // is ever pending per hook instance. When the list page
  // unmounts (user navigates away, or React strict-mode tears
  // down the component to re-mount it) the controller is aborted,
  // which fails the in-flight fetch with `AbortError` rather than
  // letting the network socket dangle until the server answers.
  const inFlightRef = useRef<AbortController | null>(null);

  const load = useCallback(async (cursor: string | null, replace: boolean) => {
    // Abort the previous in-flight request, if any, so a fast
    // resetKey change doesn't pile up two concurrent requests
    // racing for `setItems` ownership.
    if (inFlightRef.current) inFlightRef.current.abort();
    const controller = new AbortController();
    inFlightRef.current = controller;
    // Claim the paging state before the first `await`: a second
    // `loadMore()` in this tick must see the request in flight.
    pagingRef.current = beginCursorRequest(pagingRef.current);
    setLoading(true);
    setError(null);
    try {
      const result = await fetchPageRef.current(cursor, controller.signal);
      // Guard against a stale response landing after the caller
      // (or the effect) has already moved on to a new request.
      // Without this, an aborted-then-resumed request could
      // clobber the fresh data set in the meanwhile.
      if (inFlightRef.current === controller) {
        // Record the next cursor the moment the response lands, not
        // on the next render: a `loadMore()` that runs before that
        // render must ask for this page, not the one before it.
        pagingRef.current = settleCursorLoad(result.next_cursor ?? null);
        setItems((prev) => mergeCursorPage(prev, result.data, replace ? 'replace' : 'append'));
        setNextCursor(result.next_cursor ?? null);
        setTotal(result.total ?? null);
      }
    } catch (e) {
      // AbortError fires when the effect is torn down before
      // the request resolves — that's expected, and the
      // post-await `setItems` is already guarded by the
      // controller-identity check above. Swallow it.
      if ((e as Error).name !== 'AbortError' && inFlightRef.current === controller) {
        setError((e as Error).message);
      }
    } finally {
      if (inFlightRef.current === controller) {
        inFlightRef.current = null;
        // Release the claim. On success this repeats the settle from
        // the `try` block (same cursor); on failure it keeps the
        // cursor the failed page was going to replace.
        pagingRef.current = settleCursorLoad(pagingRef.current.nextCursor);
        setLoading(false);
      }
    }
  }, []);

  // Reset + initial load whenever the caller names a different list.
  useEffect(() => {
    setItems([]);
    setNextCursor(null);
    setTotal(null);
    // The list is going back to its first page: nothing is in flight
    // for it yet, and the cursor the old list's last page carried no
    // longer applies.
    pagingRef.current = settleCursorLoad(null);
    void load(null, true);
    // Cleanup: abort whatever the initial fetch is doing so a
    // quick resetKey change doesn't leak. The next render
    // (or unmount) replaces this controller.
    return () => {
      if (inFlightRef.current) inFlightRef.current.abort();
    };
  }, [load, resetKey]);

  const loadMore = useCallback(async () => {
    const cursor = nextCursorToLoad(pagingRef.current.inFlight, pagingRef.current.nextCursor);
    if (cursor === null) return;
    await load(cursor, false);
  }, [load]);

  const refresh = useCallback(async () => {
    await load(null, true);
  }, [load]);

  return {
    items,
    loading,
    error,
    hasMore: nextCursor !== null,
    total,
    loadMore,
    refresh,
    setItems,
  };
}
