/**
 * The profile seam — the single module allowed to touch the settings
 * surface's wire methods.
 *
 * Design D20/D21/D26: there is no `settings` file. The settings page is
 * the **resolved profile read backwards**; a write edits the profile
 * document through one transactional path (`profile/set`, plus the
 * capability create/delete actions), and the server re-resolves and
 * re-mounts before answering. The answer is a four-state receipt that
 * the UI must show verbatim:
 *
 * - `applied` — written and mounted; the next turn uses the new tree;
 * - `restart-required` — written, but supervision needs a process
 *   restart (the `mcp` row);
 * - `overridden` — a higher layer owns the field; nothing was written;
 * - `failed` — nothing changed.
 *
 * The five methods below are registered by
 * `crates/uva-app-server/src/rpc/methods/profile.rs` (`profile/list`
 * · `profile/get` · `profile/set` · `profile/create` ·
 * `profile/delete`), and `profile/changed` is the unscoped broadcast a
 * committed write sends. Every section goes through this file, so a
 * wire rename lands here once.
 */
import { rpc, type Unsubscribe } from './rpc';

/** One row of the resolved profile, as `profile/list` returns it. */
export interface ProfileRow {
  id: string;
  /** Which assembly segment the row sits in. */
  segment: 'core' | 'ui';
  /** Loader kind: `native` / `ui-bundle` / … */
  runtime: string;
  /** `true` / `false`, or a plugin-delegated check (D39). */
  enabled: boolean | ProfileEnableCheck;
  config: unknown;
  /** `mounted`, or `off: <reason>` when the assembly skipped it. */
  status: string;
  /** Kernel plugin state, when the row is mounted. */
  plugin_state?: string | null;
}

/** `enabled = { service = "…", check = "…", arg = "…" }` — a plugin
 *  answers whether the row assembles. */
export interface ProfileEnableCheck {
  service: string;
  check: string;
  arg?: unknown;
}

export type ProfileChangeState = 'applied' | 'restart-required' | 'overridden' | 'failed';

/** The transaction's report (D37). */
export interface ProfileChange {
  state: ProfileChangeState;
  reasons: string[];
}

/** The two capability domains `profile/create` / `profile/delete`
 *  accept (D28). */
export type CapabilityDomain = 'agent' | 'skill';

/** The `profile/changed` broadcast's payload. */
export interface ProfileChangedEvent {
  actor: string;
  state: ProfileChangeState;
  ids: string[];
  at: string | null;
}

const METHOD = {
  list: 'profile/list',
  get: 'profile/get',
  set: 'profile/set',
  create: 'profile/create',
  remove: 'profile/delete',
  changed: 'profile/changed',
} as const;

export const settingsProfile = {
  /** `profile/list` — every row with its effective status. */
  list: (signal?: AbortSignal): Promise<ProfileRow[]> =>
    rpc.request<ProfileRow[]>(METHOD.list, undefined, signal),

  /** `profile/get` — one row, or a `not_found` RpcError. */
  get: (id: string, signal?: AbortSignal): Promise<ProfileRow> =>
    rpc.request<ProfileRow>(METHOD.get, { id }, signal),

  /** `profile/set` — flip one row's enable bit. The receipt names the
   *  outcome; a caller must render it, never assume success. */
  setEnabled: (id: string, enabled: boolean, signal?: AbortSignal): Promise<ProfileChange> =>
    rpc.request<ProfileChange>(METHOD.set, { id, enabled }, signal),

  /** `profile/create` — write a capability definition to disk. */
  create: (
    domain: CapabilityDomain,
    definition: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<ProfileChange> =>
    rpc.request<ProfileChange>(METHOD.create, { domain, definition }, signal),

  /** `profile/delete` — remove a capability definition. */
  remove: (domain: CapabilityDomain, id: string, signal?: AbortSignal): Promise<ProfileChange> =>
    rpc.request<ProfileChange>(METHOD.remove, { domain, id }, signal),

  /** Subscribe to committed writes from any client. */
  onChanged: (listener: (event: ProfileChangedEvent) => void): Unsubscribe =>
    rpc.on(METHOD.changed, (params) => listener(params as ProfileChangedEvent)),
};

/**
 * The row's effective enable state as a plain boolean.
 *
 * A delegated check (D39) cannot be evaluated in the browser: the
 * kernel service answers it. Such a row is reported as enabled — that
 * is what `status` says — and the UI marks the toggle as locked
 * rather than offering a write that would replace the check with a
 * literal. Callers that need the distinction read `enabled` directly.
 */
export function rowEnabled(row: ProfileRow): boolean {
  return row.enabled !== false;
}

/** True when a plugin, not the user, decides the row's enablement. */
export function rowDelegated(row: ProfileRow): boolean {
  return typeof row.enabled === 'object' && row.enabled !== null;
}
