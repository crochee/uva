/**
 * Tasks API client — sub-agent delegations.
 *
 * Backend wires a join endpoint (`GET /api/v1/tasks`) that
 * scans recent sessions' `Agent` envelopes server-side and
 * flattens them into one list. The endpoint is keyset-paginated
 * like the rest of the v1 surface: `next_cursor` names the
 * delegation the next page starts after, and the list is ordered
 * newest-first.
 *
 * A delegation's id is `<parentSessionId>:<childSessionId>` — the
 * pair that names it. It is stable for the delegation's whole
 * life, so a URL built from it survives a refresh.
 */

import { api } from './client';
import type { List, TaskDelegation } from './types';

export interface TaskListOptions {
  /** Page size hint. The server clamps to [1, 100]. */
  limit?: number;
  /** Opaque cursor from a previous page's `next_cursor`. */
  cursor?: string;
}

export const tasksApi = {
  /** GET /api/v1/tasks — one page of delegations, newest first. */
  list: (opts: TaskListOptions = {}, signal?: AbortSignal): Promise<List<TaskDelegation>> => {
    const params = new URLSearchParams();
    if (opts.limit !== undefined) params.set('limit', String(opts.limit));
    if (opts.cursor) params.set('cursor', opts.cursor);
    const query = params.toString();
    return api.get<List<TaskDelegation>>(
      query ? `/api/v1/tasks?${query}` : '/api/v1/tasks',
      signal,
    );
  },

  /** GET /api/v1/tasks/{id} — fetch one delegation by its
   *  synthetic `<parentSessionId>:<childSessionId>` id. */
  get: (id: string, signal?: AbortSignal): Promise<TaskDelegation> =>
    api.get<TaskDelegation>(`/api/v1/tasks/${encodeURIComponent(id)}`, signal),
};
