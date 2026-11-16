/**
 * Catalog domain client — the model catalogue and the UI schema.
 *
 * Two read-only projections the shell mounts before any page runs:
 *
 * - `models/list` — the registered adapters plus the workspace
 *   default; backs the model-selector dropdown.
 * - `ui/schema` — the resolved `[ui]` profile segment as data: each
 *   row contributes a schema version and a list of components
 *   (`slot`, `kind`, `props`). The frontend renders only the kinds
 *   its registry knows; an unknown kind is shown as a placeholder,
 *   so a newer UI plugin degrades instead of breaking the panel.
 *
 * A UI bundle is JSON schema only. There is no JavaScript or CSS
 * to load here, deliberately: v1 never executes remote presentation
 * code. Both methods are registered by
 * `crates/uva-app-server/src/rpc/methods/misc.rs`.
 */
import { rpc } from './rpc';
import type {
  Cardinality,
  ModelEntry,
  ModelsResponse,
  UiComponent,
  UiSchema,
  UiSchemaResponse,
  UiSlot,
} from './types';

const METHOD = {
  models: 'models/list',
  uiSchema: 'ui/schema',
} as const;

export type {
  Cardinality,
  ModelEntry,
  ModelsResponse,
  UiComponent,
  UiSchema,
  UiSchemaResponse,
  UiSlot,
};

export const catalogApi = {
  /** `models/list` — registered models + the workspace default. */
  models: (signal?: AbortSignal): Promise<ModelsResponse> =>
    rpc.request<ModelsResponse>(METHOD.models, undefined, signal),

  /** `ui/schema` — the resolved `[ui]` segment. */
  uiSchema: (signal?: AbortSignal): Promise<UiSchemaResponse> =>
    rpc.request<UiSchemaResponse>(METHOD.uiSchema, undefined, signal),
};
