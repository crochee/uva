/**
 * Deployment plugins API client — `GET /api/v1/plugins`.
 *
 * The server's response is its boot-time plugin composition
 * (`crates/elysper-server/src/routes/plugins.rs`): the entries that
 * installed, and the entries that were skipped with the reason boot
 * recorded. It is deliberately *not* a `List` — there is nothing to
 * paginate, and which array an entry sits in is the status.
 *
 * The composition is fixed after boot, so a caller may fetch it once
 * per page mount and keep it.
 */

import { api } from './client';

/** A plugin this deployment installed. */
export interface ResolvedPlugin {
  name: string;
  /** `static` = compiled into the server binary; `remote` = a plugin
   *  process spoken to over the plugin protocol. */
  kind: 'static' | 'remote';
  /** `full` = the escalation an in-process plugin already has, or the
   *  promotion an operator granted a remote one; `reduced` is the
   *  default a remote plugin gets. */
  authority: 'reduced' | 'full';
  /** The risk the plugin's tools are judged at. A static plugin
   *  reports the worst bound its tools declare; `unknown` means it
   *  declared none (or registers no tool at all). */
  risk: 'low' | 'high' | 'unknown';
  /** The tool names it claimed. */
  tools: string[];
}

/** A configured entry that did not install. */
export interface SkippedPlugin {
  name: string;
  /** The failure the composition recorded — a spawn error, a failed
   *  handshake, an unlinked static name, a refused tool claim. */
  reason: string;
}

/** `GET /api/v1/plugins` response. */
export interface PluginsResponse {
  resolved: ResolvedPlugin[];
  skipped: SkippedPlugin[];
}

export const pluginsApi = {
  /** `GET /api/v1/plugins` — the deployment's plugin composition. */
  list: (signal?: AbortSignal): Promise<PluginsResponse> =>
    api.get<PluginsResponse>('/api/v1/plugins', signal),
};
