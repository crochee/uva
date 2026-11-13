/**
 * One declared slot, rendered where the host places it.
 *
 * A region reads the shared snapshot — it never fetches — and renders
 * each component through the registry, so an unknown kind still shows
 * the placeholder. An empty slot renders nothing: the page does not
 * reserve space for a region the deployment did not fill.
 */

import { useEffect, useSyncExternalStore, type JSX } from 'react';

import { componentsInSlot, getSnapshot, refresh, subscribe } from './store';
import { UiSchemaRenderer } from './UiSchemaRenderer';
import './ui-schema.css';

export function SlotRegion({ slot }: { slot: string }): JSX.Element | null {
  const state = useSyncExternalStore(subscribe, getSnapshot);

  useEffect(() => {
    // The store is idempotent while a request is in flight, so every
    // region mounting in the same commit produces one `ui/schema` call.
    refresh();
  }, []);

  const components = componentsInSlot(state, slot);
  if (components.length === 0) {
    return null;
  }

  return (
    <div
      className="nt-uischema__list nt-uischema__slot-region"
      data-slot={slot}
      data-testid={`slot-region-${slot}`}
    >
      {components.map((component, index) => (
        <UiSchemaRenderer
          key={`${component.slot}:${component.kind}:${index}`}
          component={component}
        />
      ))}
    </div>
  );
}
