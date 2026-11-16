/**
 * The slots this build does not place.
 *
 * A deployment may declare a slot this build has no region for; its
 * components are grouped by slot id so they stay visible rather than
 * silently dropped. When nothing is unplaced the section renders
 * nothing at all; a failed fetch reports itself here.
 */

import { useEffect, useSyncExternalStore, type JSX } from 'react';

import type { UiComponent } from '../../api/catalog';
import { Section } from '../ui/visual';
import { UiSchemaRenderer } from './UiSchemaRenderer';
import { getSnapshot, refresh, subscribe, unplacedComponents } from './store';
import './ui-schema.css';

function groupBySlot(components: UiComponent[]): Map<string, UiComponent[]> {
  const groups = new Map<string, UiComponent[]>();
  for (const component of components) {
    const group = groups.get(component.slot);
    if (group === undefined) {
      groups.set(component.slot, [component]);
    } else {
      group.push(component);
    }
  }
  return groups;
}

export function UnplacedSlots(): JSX.Element | null {
  const state = useSyncExternalStore(subscribe, getSnapshot);

  useEffect(() => {
    refresh();
  }, []);

  if (state.status === 'error') {
    return (
      <Section title="UI extensions" glyph="🧩" testId="ui-schema">
        <p className="nt-uischema__error" role="alert" data-testid="ui-schema-error">
          {`Could not read the UI schema: ${state.error}`}
        </p>
      </Section>
    );
  }

  const components = unplacedComponents(state);
  if (components.length === 0) {
    return null;
  }

  return (
    <Section title="UI extensions" glyph="🧩" testId="ui-schema">
      <div className="nt-uischema__list" data-testid="ui-schema-unplaced">
        {[...groupBySlot(components)].map(([slot, group]) => (
          <div className="nt-uischema__group" key={slot} data-slot={slot}>
            <h3 className="nt-uischema__group-title" data-testid={`ui-schema-group-${slot}`}>
              {slot}
            </h3>
            <div className="nt-uischema__list">
              {group.map((component, index) => (
                <UiSchemaRenderer
                  key={`${component.slot}:${component.kind}:${index}`}
                  component={component}
                />
              ))}
            </div>
          </div>
        ))}
      </div>
    </Section>
  );
}
