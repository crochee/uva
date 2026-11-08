/**
 * The deployment's `[ui]` extension points, rendered from schema.
 *
 * The server sends descriptions, never code. This panel asks
 * `ui/schema` once per mount and draws every component through the
 * registry; an unknown kind shows its fallback card, so a UI plugin
 * newer than this build is visible rather than fatal.
 */

import { useEffect, useState } from 'react';
import { uiSchemaApi, type UiComponent } from '../../api/ui-schema';
import { EmptyState } from '../ui/EmptyState';
import { SkeletonList } from '../ui/SkeletonList';
import { Section, StatusPill } from '../ui/visual';
import { UiSchemaRenderer } from './UiSchemaRenderer';
import './ui-schema.css';

export function UiSchemaPanel({ slot }: { slot?: string }) {
  const [components, setComponents] = useState<UiComponent[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    uiSchemaApi
      .get(controller.signal)
      .then((response) => {
        const projected = response.schemas.flatMap((schema) => schema.components);
        setComponents(slot === undefined ? projected : projected.filter((c) => c.slot === slot));
        setError(null);
      })
      .catch((cause: Error) => {
        if (cause.name === 'AbortError') return;
        // The panel is secondary on the page: a failed fetch reports
        // itself instead of taking the session down.
        setError(cause.message);
      });
    return () => controller.abort();
  }, [slot]);

  return (
    <Section
      title="UI extensions"
      glyph="🧩"
      meta={
        components !== null ? (
          <StatusPill tone={components.length > 0 ? 'blue' : 'ghost'} testId="ui-schema-count">
            {`${components.length} component${components.length === 1 ? '' : 's'}`}
          </StatusPill>
        ) : null
      }
      testId="ui-schema"
    >
      {error !== null && (
        <p className="nt-uischema__error" role="alert" data-testid="ui-schema-error">
          {`Could not read the UI schema: ${error}`}
        </p>
      )}

      {error === null && components === null && (
        <SkeletonList count={1} testId="ui-schema-skeleton" />
      )}

      {components !== null && components.length === 0 && (
        <EmptyState
          icon="🧩"
          title="No UI extensions"
          description="This deployment's [ui] profile segment declares no components."
          testId="ui-schema-empty"
        />
      )}

      {components !== null && components.length > 0 && (
        <div className="nt-uischema__list" data-testid="ui-schema-components">
          {components.map((component, index) => (
            <UiSchemaRenderer
              key={`${component.slot}:${component.kind}:${index}`}
              component={component}
            />
          ))}
        </div>
      )}
    </Section>
  );
}
