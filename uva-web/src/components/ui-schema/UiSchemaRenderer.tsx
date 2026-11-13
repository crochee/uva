/**
 * Render one server-projected UI component.
 *
 * The `kind` is looked up in [`KINDS`]; an unknown kind renders the
 * fallback instead of throwing during render.
 */

import type { UiComponent } from '../../api/ui-schema';
import { FALLBACK_KIND, KINDS } from './registry';

export function UiSchemaRenderer({ component }: { component: UiComponent }) {
  const Renderer = KINDS[component.kind] ?? FALLBACK_KIND;
  return (
    <div
      className="nt-uischema__slot"
      data-slot={component.slot}
      data-kind={component.kind}
      data-testid={`ui-schema-component-${component.kind}`}
    >
      <Renderer component={component} />
    </div>
  );
}
