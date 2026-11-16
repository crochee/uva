/**
 * Tasks domain client — sub-agent delegations.
 *
 * `tasks/list` scans recent sessions' `Agent` envelopes server-side
 * and flattens them into one list. It is keyset-paginated like every
 * other list method: `next_cursor` names the delegation the next
 * page starts after, and the list is ordered newest-first.
 *
 * A delegation's id is `<parentSessionId>:<childSessionId>` — the
 * pair that names it. It is stable for the delegation's whole
 * life, so a URL built from it survives a refresh. Both methods are
 * registered by `crates/uva-app-server/src/rpc/methods/automation.rs`.
 */
import { rpc } from './rpc';
import type { List, TaskDelegation } from './types';

const METHOD = {
  list: 'tasks/list',
  get: 'tasks/get',
} as const;

export interface TaskListOptions {
  /** Page size hint. The server clamps to [1, 100]. */
  limit?: number;
  /** Opaque cursor from a previous page's `next_cursor`. */
  cursor?: string;
}

export const tasksApi = {
  /** `tasks/list` — one page of delegations, newest first. */
  list: (opts: TaskListOptions = {}, signal?: AbortSignal): Promise<List<TaskDelegation>> =>
    rpc.request<List<TaskDelegation>>(
      METHOD.list,
      {
        ...(opts.limit !== undefined ? { limit: opts.limit } : {}),
        ...(opts.cursor ? { cursor: opts.cursor } : {}),
      },
      signal,
    ),

  /** `tasks/get` — fetch one delegation by its synthetic
   * `<parentSessionId>:<childSessionId>` id. */
  get: (id: string, signal?: AbortSignal): Promise<TaskDelegation> =>
    rpc.request<TaskDelegation>(METHOD.get, { id }, signal),
};
