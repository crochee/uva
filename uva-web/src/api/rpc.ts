/**
 * The WebSocket JSON-RPC 2.0 client.
 *
 * The app server speaks one protocol on every transport: Content-Length
 * framed JSON-RPC on stdio to the core, and one JSON-RPC envelope per
 * WebSocket text message at `/rpc` to the browser. This module is
 * the browser's half: it owns the single socket, correlates
 * responses to requests by id, and fans notifications out to
 * subscribers. The browser never sees a `Content-Length` header.
 *
 * # Wire shape
 *
 * A request is `{"jsonrpc":"2.0","id":"c1","method":"sessions/list",
 * "params":{...}}`; the answer is `{"jsonrpc":"2.0","id":"c1",
 * "result":...}` or `{"jsonrpc":"2.0","id":"c1","error":{"code":-32000,
 * "message":"...","data":{"code":"not_found"}}}`. Notifications carry
 * no id: `{"jsonrpc":"2.0","method":"session/event","params":{...}}`.
 *
 * # Reconnection
 *
 * Requests in flight when the socket drops reject with
 * `RpcTransportError`; callers decide whether to retry. The socket
 * itself reconnects with capped exponential backoff, and
 * `onConnection` listeners are told about every state change — the
 * chat page uses that to re-`follow` from its last cursor, which
 * rebuilds the timeline without a gap (the baseline replays every
 * row after `from`).
 */

/** A request-level failure: the server answered with an error. */
export class RpcError extends Error {
  /** The machine code from `error.data.code`, or `'error'`. */
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = 'RpcError';
    this.code = code;
  }
}

/** The socket died before the request was answered. */
export class RpcTransportError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'RpcTransportError';
  }
}

/** One JSON-RPC error object as the server writes it. */
interface WireError {
  code?: number;
  message?: string;
  data?: { code?: string };
}

/**
 * `error.data.code` is the contract — the machine-readable reason
 * (`"not_found"`, `"validation"`, …); the numeric `code` is always
 * `-32000` for application errors and carries no information.
 */
function wireCode(error: WireError): string {
  const data = error.data;
  if (data && typeof data.code === 'string' && data.code.length > 0) {
    return data.code;
  }
  return 'error';
}

/**
 * The `error.data.code` values the session / approval surface can
 * answer with. The numeric `code` is always `-32000` for application
 * errors and carries no information, so a caller branches on these.
 */
export type RpcErrorCode =
  /** Unknown session, method target, or `approval_id`. */
  | 'not_found'
  /** Malformed params. */
  | 'validation'
  /** The core restarted (or is not up): the request was not delivered. */
  | 'core_unavailable'
  /** The log moved on since the resume token was issued. */
  | 'resume_conflict'
  /** The suspended round has no request envelope to resume. */
  | 'resume_no_snapshot'
  /** The decision set omits an open interrupt. */
  | 'incomplete_resume';

/**
 * The core's projection of a durable suspension, sent as an
 * `approval/request` **notification**.
 *
 * It is session-scoped like `session/event`: the gateway routes it to
 * the connections following `session_id` and does not rewrite the
 * payload. `approval_id` is the UI correlation id the durable log
 * keeps — it is not the JSON-RPC `id`, so responding is not "replying
 * to" the notification.
 */
export interface ApprovalRequestParams {
  /** Correlation id shown by the UI; the log keeps it. */
  approval_id: string;
  /** The session whose follower receives the request. */
  session_id: string;
  /** The tool call a decision would let run. */
  tool: string;
  /** `unknown` is a policy that could not classify the call. */
  risk: 'low' | 'high' | 'unknown';
  /** A short rendering of what the call would do. */
  preview?: string;
}

/** What an operator decided about one held call. */
export type ApprovalDecision = 'allow' | 'deny' | 'allow_session';

/**
 * `approval/respond` — an ordinary client request.
 *
 * The gateway forwards it upstream unchanged; the core maps it onto
 * its durable `sessions/resume` + `ResumeToken` path, which is where
 * the decision becomes truth. A timeout on that path defaults to deny.
 */
export interface ApprovalRespondParams {
  approval_id: string;
  session_id: string;
  decision: ApprovalDecision;
  /** Free-form detail (a denial reason, a grant scope, …). */
  reason?: string;
}

/** The notifications the gateway routes by `params.session_id`. */
export type SessionScopedNotification =
  'session/event' | 'anthropic/messages/event' | 'approval/request';

/** The `session/event` increments the core emits. */
export type SessionEventKind = 'row' | 'live' | 'interrupts';

/** What every `session/event` payload carries; `kind` selects the rest. */
export interface SessionEventParams {
  session_id: string;
  kind: SessionEventKind;
}

/** A notification listener's registration handle. */
export interface Unsubscribe {
  (): void;
}

/** Connection states the client reports to `onConnection`. */
export type ConnectionState = 'connecting' | 'open' | 'closed';

/** Everything a listener needs to decide what to rebuild. */
export interface ConnectionChange {
  state: ConnectionState;
  /** True when this transition is a reconnection, not the first open. */
  reopened: boolean;
}

type PendingEntry = {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
};

type ConnectionListener = (change: ConnectionChange) => void;

type NotificationListener = (params: unknown) => void;

/** Backoff before the first retry. */
const FIRST_BACKOFF_MS = 250;
/** Backoff ceiling — a dead server is polled at 2s, not faster. */
const MAX_BACKOFF_MS = 2_000;

export class RpcClient {
  private socket: WebSocket | null = null;
  private nextId = 0;
  private pending = new Map<string, PendingEntry>();
  private listeners = new Map<string, Set<NotificationListener>>();
  private connectionListeners = new Set<ConnectionListener>();
  private backoff = FIRST_BACKOFF_MS;
  private openedOnce = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private connectPromise: Promise<WebSocket> | null = null;
  private closedByCaller = false;

  /**
   * Issue one request and await its result.
   *
   * Opens the socket on first use; subsequent calls share it. The
   * `signal` aborts the *wait*, not the protocol exchange — the
   * server still finishes the work; only this caller stops caring.
   */
  request<T>(method: string, params?: unknown, signal?: AbortSignal): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const id = `c${++this.nextId}`;
      const settle = (error: Error | null, value?: unknown) => {
        this.pending.delete(id);
        if (signal) signal.removeEventListener('abort', onAbort);
        if (error) reject(error);
        else resolve(value as T);
      };
      const onAbort = () => settle(new DOMException('aborted', 'AbortError'));
      if (signal) {
        if (signal.aborted) {
          onAbort();
          return;
        }
        signal.addEventListener('abort', onAbort, { once: true });
      }
      this.pending.set(id, {
        resolve: (value) => settle(null, value),
        reject: (error) => settle(error),
      });
      this.connect()
        .then((socket) => {
          socket.send(
            JSON.stringify({
              jsonrpc: '2.0',
              id,
              method,
              ...(params === undefined ? {} : { params }),
            }),
          );
        })
        .catch((error) => {
          settle(error instanceof Error ? error : new RpcTransportError(String(error)));
        });
    });
  }

  /**
   * Subscribe to one notification method (`'session/event'`, …).
   *
   * Filtering by payload — a session id, say — is the listener's
   * job: the bus is deliberately dumb so it never has to understand
   * a method's params.
   */
  on(method: string, listener: NotificationListener): Unsubscribe {
    let set = this.listeners.get(method);
    if (!set) {
      set = new Set();
      this.listeners.set(method, set);
    }
    set.add(listener);
    return () => {
      set.delete(listener);
      if (set.size === 0) this.listeners.delete(method);
    };
  }

  /** Observe socket lifecycle changes (`reopen` = re-follow time). */
  onConnection(listener: ConnectionListener): Unsubscribe {
    this.connectionListeners.add(listener);
    return () => this.connectionListeners.delete(listener);
  }

  /**
   * The current socket, opening it if needed.
   *
   * Concurrent callers share one connect attempt; a failure resets
   * the promise so the next call retries (with backoff).
   */
  connect(): Promise<WebSocket> {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      return Promise.resolve(this.socket);
    }
    if (this.connectPromise) return this.connectPromise;
    const attempt = new Promise<WebSocket>((resolve, reject) => {
      this.emitConnection('connecting');
      const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const socket = new WebSocket(`${proto}//${window.location.host}/rpc`);
      let settled = false;
      this.socket = socket;
      socket.onopen = () => {
        if (settled) return;
        settled = true;
        this.backoff = FIRST_BACKOFF_MS;
        const reopened = this.openedOnce;
        this.openedOnce = true;
        this.emitConnection('open', reopened);
        resolve(socket);
      };
      socket.onmessage = (event) => this.receive(event.data);
      socket.onclose = () => {
        this.failPending(new RpcTransportError('the RPC connection dropped'));
        this.emitConnection('closed');
        if (!settled) {
          // The socket died before opening: fail the connect attempt
          // so its `catch` resets the promise and retries (with
          // backoff) — otherwise the first failed connect hangs
          // forever and the app never recovers.
          settled = true;
          reject(new RpcTransportError('the RPC socket failed to open'));
          return;
        }
        if (!this.closedByCaller) this.scheduleReconnect();
      };
      socket.onerror = () => {
        // `onclose` follows; nothing to do here.
      };
    });
    this.connectPromise = attempt;
    attempt.catch(() => {
      this.connectPromise = null;
      if (!this.closedByCaller) this.scheduleReconnect();
    });
    return attempt;
  }

  /** Stop reconnecting and close the socket (tests / teardown). */
  close(): void {
    this.closedByCaller = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket) {
      this.socket.close();
      this.socket = null;
    }
  }

  /** One envelope arrived: route it to a pending request or a bus. */
  private receive(raw: unknown): void {
    if (typeof raw !== 'string') return;
    let envelope: {
      id?: string | number;
      result?: unknown;
      error?: WireError;
      method?: string;
      params?: unknown;
    };
    try {
      envelope = JSON.parse(raw);
    } catch {
      return; // A malformed frame must not kill the socket.
    }
    if (envelope.method !== undefined) {
      const set = this.listeners.get(envelope.method);
      if (set) {
        for (const listener of set) listener(envelope.params);
      }
      return;
    }
    if (envelope.id !== undefined) {
      const entry = this.pending.get(String(envelope.id));
      if (!entry) return;
      if (envelope.error) {
        const error = envelope.error;
        entry.reject(
          new RpcError(
            wireCode(error),
            typeof error.message === 'string' ? error.message : 'request failed',
          ),
        );
      } else {
        entry.resolve(envelope.result);
      }
    }
  }

  /** Reject everything in flight — the socket they were sent on is gone. */
  private failPending(error: Error): void {
    for (const entry of this.pending.values()) entry.reject(error);
    this.pending.clear();
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer !== null) return;
    const delay = this.backoff;
    this.backoff = Math.min(this.backoff * 2, MAX_BACKOFF_MS);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connectPromise = null;
      this.connect().catch(() => {
        // `connect` already scheduled the next attempt.
      });
    }, delay);
  }

  private emitConnection(state: ConnectionState, reopened = false): void {
    for (const listener of this.connectionListeners) {
      listener({ state, reopened });
    }
  }
}

/** The single shared client. Every module imports this instance. */
export const rpc = new RpcClient();
