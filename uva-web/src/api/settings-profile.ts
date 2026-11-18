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
import type { ProfileChange, ProfileChangedEvent, ProfileRow } from './types';

/**
 * The profile wire types are generated: `ProfileRow`,
 * `ProfileEnableCheck`, `ProfileChange`, `ProfileChangeState` and
 * `ProfileChangedEvent` are derived from
 * `crates/uva-app-server/src/api/v1/profile.rs`, so a rename there is a
 * rename here and the emitter's gate catches a drift. They are
 * re-exported because this module is the settings surface's single entry
 * point.
 */
export type { ProfileChange, ProfileChangeState, ProfileChangedEvent, ProfileRow } from './types';

/** The two capability domains `profile/create` / `profile/delete`
 *  accept (D28). */
export type CapabilityDomain = 'agent' | 'skill';

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
    rpc.request<ProfileChange>(METHOD.set, { id, disabled: !enabled }, signal),

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
 * The row carries a plain `disabled` bit (the dsh entry model); a row
 * inside a disabled group is reported by `status` instead.
 */
export function rowEnabled(row: ProfileRow): boolean {
  return !row.disabled;
}
