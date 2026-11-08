/**
 * The v1 surface, spoken over the JSON-RPC socket.
 *
 * Every page and hook kept its REST vocabulary (`api.get('/api/v1/
 * skills')`, `useCursorList('/api/v1/sessions')`); the server no
 * longer speaks REST — it speaks JSON-RPC on a WebSocket at `/rpc`
 * (see `rpc.ts`). This module is the seam: it translates each
 * versioned REST path onto the RPC method that replaced it, so the
 * call sites did not have to change with the transport.
 *
 * The mapping is total and static — one table, every path the app
 * uses. A path outside the table is a programming error and throws
 * immediately (better than a silent no-op against a dead transport).
 *
 * Errors: the RPC error envelope (`error.data.code`) becomes the
 * same [`ApiError`] the REST client produced (`err.code`, kept;
 * `err.status`, derived for the handful of call sites that still
 * branch on a number).
 */

import { RpcError, rpc } from './rpc';

/**
 * The credential every surface authenticates with, or `null` when the
 * deployment runs without one.
 *
 * The app server is single-user local now: it authenticates nobody
 * and mints every session for the one operator. The API key UI is
 * gone with the auth wall; this export stays (type-compatible) for
 * the call sites that still reference it.
 */
export function resolveApiKey(): string | null {
  return null;
}

/** Structured error thrown by [`ApiClient`] when a request fails. */
export class ApiError extends Error {
  /** HTTP-equivalent status, derived from the machine code. */
  readonly status: number;
  /** The machine code (`"not_found"`, `"validation"`, …). */
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

/**
 * Map a machine code onto the HTTP status it used to travel as, so
 * call sites that branch on `err.status` keep their meaning
 * (`404` → permanent, `410` → stale cursor, `5xx` → transient).
 */
const STATUS_BY_CODE: Record<string, number> = {
  not_found: 404,
  follow_cursor_truncated: 410,
  validation: 400,
  bad_request: 400,
  conflict: 409,
  invalid_model: 400,
  session_log_read: 500,
};

interface WireParams {
  [key: string]: unknown;
}

/** A path-to-method row: what the REST call becomes on the socket. */
interface Route {
  method: string;
  /** Build the RPC params from the REST path/body/query. */
  params: (parts: string[], query: URLSearchParams, body: unknown) => WireParams;
}

/**
 * Query params the list endpoints accept (`cursor`, `limit`,
 * `sort`), passed through as-is — `useCursorList` builds exactly
 * these. `limit` keeps its string form: the server's `optional_u64`
 * deserializer accepts both.
 */
function pageParams(query: URLSearchParams): WireParams {
  const params: WireParams = {};
  for (const key of ['cursor', 'limit', 'sort']) {
    const value = query.get(key);
    if (value !== null) params[key] = value;
  }
  return params;
}

/** `{id: parts[0]}` — the one-segment resource detail. */
const byId: Route = {
  method: '',
  params: (parts) => ({ id: parts[0] }),
};

/** `{name: parts[0]}` — the name-keyed resource detail. */
const byName: Route = {
  method: '',
  params: (parts) => ({ name: parts[0] }),
};

/**
 * Every REST path the app calls, mapped onto its RPC method.
 *
 * Keyed by the path template with `:x` marking each segment; `*`
 * marks a query suffix the route ignores (params carry the meaning).
 */
const ROUTES: Record<string, Route> = {
  // Sessions
  'GET /api/v1/sessions': { method: 'sessions/list', params: (_p, query) => pageParams(query) },
  'GET /api/v1/sessions/:id': { ...byId, method: 'sessions/get' },
  'DELETE /api/v1/sessions/:id': { ...byId, method: 'sessions/delete' },
  'GET /api/v1/sessions/:id/status': { ...byId, method: 'sessions/status' },
  'GET /api/v1/sessions/:id/usage': {
    method: 'sessions/usage',
    params: (parts) => ({ session_id: parts[0] }),
  },
  'GET /api/v1/sessions/:id/export': { ...byId, method: 'sessions/export' },
  'POST /api/v1/sessions/:id/fork': {
    method: 'sessions/fork',
    params: (parts, _q, body) => ({ id: parts[0], ...(body as object) }),
  },
  'GET /api/v1/sessions/:id/events': { ...byId, method: 'sessions/events' },
  'POST /api/v1/sessions/:id/resume': {
    method: 'sessions/resume',
    params: (parts, _q, body) => ({ session_id: parts[0], ...((body as object) ?? {}) }),
  },
  'POST /api/v1/sessions/:id/compact': {
    method: 'sessions/compact',
    params: (parts) => ({ session_id: parts[0] }),
  },
  // Chat
  'POST /api/v1/chat/sessions': {
    method: 'chat/session/create',
    params: (_p, _q, body) => ({ ...((body as object) ?? {}) }),
  },
  'POST /api/v1/chat/sessions/:id/messages': {
    method: 'chat/message/send',
    params: (parts, _q, body) => ({ session_id: parts[0], ...((body as object) ?? {}) }),
  },
  'POST /api/v1/chat/sessions/:id/cancel': {
    method: 'chat/cancel',
    params: (parts) => ({ session_id: parts[0] }),
  },
  'POST /api/v1/chat/sessions/:id/regenerate': {
    method: 'chat/regenerate',
    params: (parts) => ({ session_id: parts[0] }),
  },
  'POST /api/v1/chat/messages/:id/feedback': {
    method: 'chat/feedback',
    params: (parts, _q, body) => ({ message_id: parts[0], ...((body as object) ?? {}) }),
  },
  // Agents
  'GET /api/v1/agents': { method: 'agents/list', params: (_p, query) => pageParams(query) },
  'GET /api/v1/agents/default': { method: 'agents/default', params: () => ({}) },
  'POST /api/v1/agents': {
    method: 'agents/create',
    params: (_p, _q, body) => ({ ...((body as object) ?? {}) }),
  },
  'GET /api/v1/agents/:name': { ...byName, method: 'agents/get' },
  'DELETE /api/v1/agents/:name': { ...byName, method: 'agents/delete' },
  // Skills
  'GET /api/v1/skills': { method: 'skills/list', params: (_p, query) => pageParams(query) },
  'POST /api/v1/skills': {
    method: 'skills/create',
    params: (_p, _q, body) => ({ ...((body as object) ?? {}) }),
  },
  'POST /api/v1/skills/reload': { method: 'skills/reload', params: () => ({}) },
  'GET /api/v1/skills/:name': { ...byName, method: 'skills/get' },
  'DELETE /api/v1/skills/:name': { ...byName, method: 'skills/delete' },
  // Tools
  'GET /api/v1/tools': { method: 'tools/list', params: (_p, query) => pageParams(query) },
  'POST /api/v1/tools': {
    method: 'tools/register',
    params: (_p, _q, body) => ({ ...((body as object) ?? {}) }),
  },
  'GET /api/v1/tools/:name': { ...byName, method: 'tools/get' },
  'DELETE /api/v1/tools/:name': { ...byName, method: 'tools/unregister' },
  // Tasks
  'GET /api/v1/tasks': { method: 'tasks/list', params: (_p, query) => pageParams(query) },
  'GET /api/v1/tasks/:id': { ...byId, method: 'tasks/get' },
  // Schedules
  'GET /api/v1/schedules': { method: 'schedules/list', params: (_p, query) => pageParams(query) },
  'POST /api/v1/schedules': {
    method: 'schedules/create',
    params: (_p, _q, body) => ({ ...((body as object) ?? {}) }),
  },
  'GET /api/v1/schedules/:id': { ...byId, method: 'schedules/get' },
  'PATCH /api/v1/schedules/:id': {
    method: 'schedules/patch',
    params: (parts, _q, body) => ({ id: parts[0], ...((body as object) ?? {}) }),
  },
  'DELETE /api/v1/schedules/:id': { ...byId, method: 'schedules/delete' },
  'POST /api/v1/schedules/:id/tick': { ...byId, method: 'schedules/tick' },
  // Workflows
  'GET /api/v1/workflows': { method: 'workflows/list', params: (_p, query) => pageParams(query) },
  'POST /api/v1/workflows': {
    method: 'workflows/create',
    params: (_p, _q, body) => ({ ...((body as object) ?? {}) }),
  },
  'GET /api/v1/workflows/:id': { ...byId, method: 'workflows/get' },
  'PUT /api/v1/workflows/:id': {
    method: 'workflows/replace',
    params: (parts, _q, body) => ({ id: parts[0], ...((body as object) ?? {}) }),
  },
  'DELETE /api/v1/workflows/:id': { ...byId, method: 'workflows/delete' },
  'POST /api/v1/workflows/:id/run': { ...byId, method: 'workflows/run' },
  // Evals
  'GET /api/v1/evals': { method: 'evals/list', params: (_p, query) => pageParams(query) },
  'POST /api/v1/evals': {
    method: 'evals/create',
    params: (_p, _q, body) => ({ ...((body as object) ?? {}) }),
  },
  'GET /api/v1/evals/:name': { ...byName, method: 'evals/get' },
  'DELETE /api/v1/evals/:name': { ...byName, method: 'evals/delete' },
  'POST /api/v1/evals/:name/run': { ...byName, method: 'evals/run' },
  // Catalogues
  'GET /api/v1/models': { method: 'models/list', params: () => ({}) },
  'GET /api/v1/plugins': { method: 'plugins/list', params: () => ({}) },
  'GET /api/v1/ui/schema': { method: 'ui/schema', params: () => ({}) },
  'GET /api/v1/search': {
    method: 'search/all',
    params: (_p, query) => {
      const params: WireParams = { q: query.get('q') ?? '' };
      const limit = query.get('limit');
      if (limit !== null) params.limit = Number(limit);
      const domains = query.get('domains');
      if (domains !== null) params.domains = domains;
      return params;
    },
  },
};

/** Resolve `GET /api/v1/sessions/abc?x` to its table row. */
function resolveRoute(
  method: string,
  path: string,
): { route: Route; parts: string[]; query: URLSearchParams } {
  const [pathname, search = ''] = path.split('?');
  const query = new URLSearchParams(search);
  const segments = pathname
    .replace(/^\/api\/v1\//, '')
    .split('/')
    .filter((s) => s.length > 0);
  // Try the longest concrete template first so `/agents/default`
  // wins over `/agents/:name`. Variable segments become `:id`, the
  // placeholder the table keys use.
  for (let depth = segments.length; depth >= 0; depth -= 1) {
    // The segments that replace `:id` placeholders, in order — the
    // route's `params` builder indexes into these (`parts[0]` is the
    // id in `/sessions/<id>/messages` as well as `/sessions/<id>`).
    const parts: string[] = [];
    const tail = segments
      .slice(0, depth)
      .map((segment) => {
        if (/^[a-z_-]+$/.test(segment)) return segment;
        parts.push(segment);
        return ':id';
      })
      .join('/');
    const template = tail.length > 0 ? `${method} /api/v1/${tail}` : `${method} /api/v1`;
    const route = ROUTES[template];
    if (route) {
      return { route, parts, query };
    }
  }
  throw new Error(`no RPC method maps ${method} ${pathname}`);
}

class ApiClient {
  async get<T>(path: string, signal?: AbortSignal): Promise<T> {
    return this.request<T>('GET', path, undefined, signal);
  }

  async post<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    return this.request<T>('POST', path, body, signal);
  }

  async put<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    return this.request<T>('PUT', path, body, signal);
  }

  async patch<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    return this.request<T>('PATCH', path, body, signal);
  }

  /** DELETE — resolves with `undefined` on success; throws on error. */
  async del(path: string, signal?: AbortSignal): Promise<void> {
    await this.request<unknown>('DELETE', path, undefined, signal);
  }

  /**
   * GET a non-JSON body — an export artifact — with the same error
   * handling the JSON methods use.
   *
   * On the RPC wire the artifact arrives base64-encoded beside its
   * media type; this reconstitutes the `Response` the caller (a
   * download) expects, so the disposition stays the caller's.
   */
  async getRaw(path: string, signal?: AbortSignal): Promise<Response> {
    const artifact = await this.request<{
      content_type: string;
      content_base64: string;
    }>('GET', path, undefined, signal);
    const bytes = Uint8Array.from(atob(artifact.content_base64), (ch) => ch.charCodeAt(0));
    return new Response(new Blob([bytes], { type: artifact.content_type }));
  }

  private async request<T>(
    method: string,
    path: string,
    body: unknown,
    signal?: AbortSignal,
  ): Promise<T> {
    const { route, parts, query } = resolveRoute(method, path);
    try {
      return (await rpc.request<T>(route.method, route.params(parts, query, body), signal)) as T;
    } catch (error) {
      if (error instanceof RpcError) {
        throw new ApiError(STATUS_BY_CODE[error.code] ?? 500, error.code, error.message);
      }
      throw error;
    }
  }
}

export const api = new ApiClient();
