/**
 * Agents domain client — the agent catalogue surface.
 *
 * Agents are registered from files, not from profile rows: `agents/list`
 * is the catalogue as loaded from `.agents/agents`, and
 * `agents/default` names the built-in agent the server falls back to.
 * Both methods are registered by
 * `crates/uva-app-server/src/rpc/methods/agents.rs`.
 */
import { rpc } from './rpc';
import type { AgentDetail, List } from './types';

const METHOD = {
  list: 'agents/list',
  default: 'agents/default',
} as const;

export interface AgentListParams {
  /** Page size hint. The server clamps to [1, 100]. */
  limit?: number;
  /** Opaque cursor from a previous page's `next_cursor`. */
  cursor?: string;
  /** Sort field; prefix with `-` for descending. */
  sort?: string;
}

/** The `agents/default` answer: the built-in agent's name and where it
 *  came from. */
export interface DefaultAgent {
  name: string;
  source: string;
}

export const agentsApi = {
  /** `agents/list` — one page of the agent catalogue. */
  list: (params: AgentListParams = {}, signal?: AbortSignal): Promise<List<AgentDetail>> =>
    rpc.request<List<AgentDetail>>(
      METHOD.list,
      {
        ...(params.limit !== undefined ? { limit: params.limit } : {}),
        ...(params.cursor ? { cursor: params.cursor } : {}),
        ...(params.sort ? { sort: params.sort } : {}),
      },
      signal,
    ),

  /** `agents/default` — the built-in agent the composer starts on. */
  defaultAgent: (signal?: AbortSignal): Promise<DefaultAgent> =>
    rpc.request<DefaultAgent>(METHOD.default, undefined, signal),
};
