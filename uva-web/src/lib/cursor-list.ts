/**
 * Pure core of `useCursorList`.
 *
 * The hook itself is a React hook: it cannot run under vitest's
 * node environment (no renderer, no DOM). The rules that decide
 * what a list page shows — how one response is folded into the
 * accumulated items, and when `loadMore()` is allowed to fire —
 * are pure and live here, so they can be proven without a browser
 * (`docs/adr/0001-frontend-test-policy.md`).
 *
 * This module stays React-free on purpose: `src/lib/*` is the
 * convention for modules a plain node test can import.
 */

/**
 * How a response is folded into the accumulated items.
 *
 * - `replace`: the response IS the whole list (the first page, or
 *   a refresh after the path or the query options changed) — the
 *   items loaded so far are dropped.
 * - `append`: the response is the next page — it goes after the
 *   items already loaded, in order.
 */
export type CursorPageMode = 'replace' | 'append';

/**
 * Fold one response into the accumulated items.
 *
 * Returns a fresh array in both modes: the caller (React state)
 * needs a new reference, and neither input is mutated.
 */
export function mergeCursorPage<T>(
  current: readonly T[],
  incoming: readonly T[],
  mode: CursorPageMode,
): T[] {
  if (mode === 'replace') return [...incoming];
  return [...current, ...incoming];
}

/**
 * The cursor `loadMore()` should request — or `null` when the call
 * must be a no-op.
 *
 * A second page is asked for only when no request is in flight and
 * the list has a next page. The hook aborts the previous request
 * before starting a new one, so a concurrent second fetch would
 * race for the items; and a list without a next page has nothing
 * to ask for. An empty cursor counts as "no next page" — the server
 * never mints one, and an empty string would fetch the first page
 * again.
 */
export function nextCursorToLoad(loading: boolean, nextCursor: string | null): string | null {
  if (loading) return null;
  if (!nextCursor) return null;
  return nextCursor;
}

/**
 * The paging state of a cursor list: which page comes next, and
 * whether a request for it is in flight.
 *
 * The hook owns this synchronously — a request claims it the moment
 * it starts and a response settles it the moment it lands — never at
 * render time. The `loadMore()` guard reads it, so its answer cannot
 * be one render behind what the list is actually doing.
 */
export interface CursorPaging {
  /** The cursor the next page request must ask for; `null` when the list has no next page. */
  readonly nextCursor: string | null;
  /** Whether a page request is in flight right now. */
  readonly inFlight: boolean;
}

/**
 * Claim the paging state for a request that is starting now.
 *
 * Called before the fetch's first `await`, so a second `loadMore()`
 * in the same tick — before React re-renders — already sees the
 * request in flight and is a no-op instead of a second fetch of the
 * same page.
 */
export function beginCursorRequest(paging: CursorPaging): CursorPaging {
  return { ...paging, inFlight: true };
}

/**
 * Release the claim when a request settles, and record the cursor
 * the next `loadMore()` must ask for.
 *
 * `nextCursor` is `null` when the list ran out of pages, or when the
 * list is being reset to its first page. Recording it here — not on
 * the next render — is what keeps a `loadMore()` that runs before
 * that render from asking for the page that just landed again.
 */
export function settleCursorLoad(nextCursor: string | null): CursorPaging {
  return { nextCursor, inFlight: false };
}
