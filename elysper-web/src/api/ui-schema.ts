/**
 * UI schema API client — `ui/schema`.
 *
 * The server projects its resolved `[ui]` profile segment as data:
 * each row contributes a schema version and a list of components
 * (`slot`, `kind`, `props`). The frontend renders only the kinds its
 * registry knows; an unknown kind is shown as a placeholder, so a
 * newer UI plugin degrades instead of breaking the panel.
 *
 * A UI bundle is JSON schema only. There is no JavaScript or CSS to
 * load here, deliberately: v1 never executes remote presentation code.
 */

import { api } from './client';

/** One component a UI row places in a slot. */
export interface UiComponent {
  /** Where the component renders, e.g. `chat.sidebar`. */
  slot: string;
  /** The registered renderer kind. */
  kind: string;
  /** The renderer's props (a JSON object). */
  props: Record<string, unknown>;
}

/** One UI row's rendered description. */
export interface UiSchema {
  /** The schema version; v1 is the current one. */
  version: number;
  components: UiComponent[];
}

/** `ui/schema` response body. */
export interface UiSchemaResponse {
  schemas: UiSchema[];
}

export const uiSchemaApi = {
  /** `ui/schema` — the resolved `[ui]` segment. */
  get: (signal?: AbortSignal): Promise<UiSchemaResponse> =>
    api.get<UiSchemaResponse>('/api/v1/ui/schema', signal),
};
