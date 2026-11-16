/**
 * Skills domain client — the skill catalogue surface.
 *
 * A skill's body lives in the plugin layout (`.agents/skills/<name>`);
 * the profile has one row for the whole surface, not one per skill.
 * The two methods the settings page needs are registered by
 * `crates/uva-app-server/src/rpc/methods/agents.rs` (`skills/list` ·
 * `skills/reload`).
 */
import { rpc } from './rpc';
import type { List, ReloadSkillsResponse, Skill } from './types';

const METHOD = {
  list: 'skills/list',
  reload: 'skills/reload',
} as const;

export const skillsApi = {
  /** `skills/list` — the loaded skills, one row each. */
  list: (signal?: AbortSignal): Promise<List<Skill>> =>
    rpc.request<List<Skill>>(METHOD.list, { limit: 100 }, signal),

  /** `skills/reload` — re-read the skills directory; the answer
   * counts what came back up. */
  reload: (signal?: AbortSignal): Promise<ReloadSkillsResponse> =>
    rpc.request<ReloadSkillsResponse>(METHOD.reload, undefined, signal),
};
