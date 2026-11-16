/**
 * Search domain client — the cross-domain search engine.
 *
 * `search/all` runs the same BM25 + vector engine the agent's `search`
 * tool uses and returns ranked hits from every catalogue. It is
 * registered by `crates/uva-app-server/src/rpc/methods/misc.rs`; the
 * param shape is `crates/uva-app-server/src/routes/search.rs`.
 */
import { rpc } from './rpc';
import type { List, SearchHit } from './types';

const METHOD = {
  all: 'search/all',
} as const;

export interface SearchOptions {
  /** Maximum hits. Default 20, capped at 50. */
  limit?: number;
  /** Comma-separated domain filter
   *  (`tool,mcp,skill,memory,agent,session`); absent searches every
   *  domain. */
  domains?: string;
}

export const searchApi = {
  /** `search/all` — ranked hits for `query`, best-first. */
  all: (query: string, opts: SearchOptions = {}, signal?: AbortSignal): Promise<List<SearchHit>> =>
    rpc.request<List<SearchHit>>(
      METHOD.all,
      {
        q: query,
        ...(opts.limit !== undefined ? { limit: opts.limit } : {}),
        ...(opts.domains ? { domains: opts.domains } : {}),
      },
      signal,
    ),
};
