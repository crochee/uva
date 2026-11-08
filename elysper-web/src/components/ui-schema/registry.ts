/**
 * The kind → renderer registry.
 *
 * The server validates the schema shape but deliberately does not
 * reject an unknown `kind`: the fallback belongs here, where a newer
 * component can be added without a server change.
 */

import type { ComponentType } from 'react';
import type { UiComponent } from '../../api/ui-schema';
import { KeyValueKind, ListKind, PanelKind, UnknownKind } from './kinds';

export interface RendererProps {
  component: UiComponent;
}

/** Every kind this build renders. */
export const KINDS: Record<string, ComponentType<RendererProps>> = {
  panel: PanelKind,
  list: ListKind,
  'key-value': KeyValueKind,
};

/** The fallback an unknown kind resolves to. */
export const FALLBACK_KIND: ComponentType<RendererProps> = UnknownKind;
