/**
 * The component kinds the web build ships.
 *
 * Every renderer reads JSON props only: no `dangerouslySetInnerHTML`,
 * no script tag, no stylesheet import. A kind a deployment ships but
 * this build does not know falls to [`UnknownKind`], which shows the
 * raw props rather than dropping the panel.
 */

import type { UiComponent } from '../../api/ui-schema';

export interface KindProps {
  component: UiComponent;
}

/** Read an optional string prop (three kinds share this shape). */
function text(props: Record<string, unknown>, key: string): string | null {
  const value = props[key];
  return typeof value === 'string' ? value : null;
}

/** A titled prose panel. */
export function PanelKind({ component }: KindProps) {
  const title = text(component.props, 'title');
  const body = text(component.props, 'body');
  const raw = component.props.items;
  const lines = Array.isArray(raw)
    ? raw.filter((item): item is string => typeof item === 'string')
    : [];

  return (
    <article className="nt-uischema__card" data-testid="ui-schema-panel">
      {title !== null && <h3 className="nt-uischema__title">{title}</h3>}
      {body !== null && <p className="nt-uischema__body">{body}</p>}
      {lines.length > 0 && (
        <ul className="nt-uischema__items">
          {lines.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      )}
    </article>
  );
}

/** A title plus an item list. */
export function ListKind({ component }: KindProps) {
  const title = text(component.props, 'title');
  const raw = component.props.items;
  const lines = Array.isArray(raw)
    ? raw.filter((item): item is string => typeof item === 'string')
    : [];

  return (
    <section className="nt-uischema__card" data-testid="ui-schema-list">
      {title !== null && <h3 className="nt-uischema__title">{title}</h3>}
      {lines.length === 0 ? (
        <p className="nt-uischema__muted">No items.</p>
      ) : (
        <ul className="nt-uischema__items">
          {lines.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** A title plus a label/value table. */
export function KeyValueKind({ component }: KindProps) {
  const title = text(component.props, 'title');
  const raw = component.props.entries;
  const entries =
    raw !== null && typeof raw === 'object' && !Array.isArray(raw)
      ? Object.entries(raw as Record<string, unknown>)
      : [];

  return (
    <section className="nt-uischema__card" data-testid="ui-schema-key-value">
      {title !== null && <h3 className="nt-uischema__title">{title}</h3>}
      {entries.length === 0 ? (
        <p className="nt-uischema__muted">No entries.</p>
      ) : (
        <dl className="nt-uischema__pairs">
          {entries.map(([key, value]) => (
            <div className="nt-uischema__pair" key={key}>
              <dt>{key}</dt>
              <dd>{String(value)}</dd>
            </div>
          ))}
        </dl>
      )}
    </section>
  );
}

/**
 * The unknown-kind fallback.
 *
 * A deployment whose UI plugin is newer than this build still reaches
 * the panel: the slot, kind, and raw props are visible, and the rest of
 * the chat surface is unaffected.
 */
export function UnknownKind({ component }: KindProps) {
  return (
    <section
      className="nt-uischema__card nt-uischema__card--unknown"
      data-testid="ui-schema-unknown"
    >
      <h3 className="nt-uischema__title">{`Unsupported component: ${component.kind}`}</h3>
      <p className="nt-uischema__muted">
        {`This build has no renderer for slot "${component.slot}".`}
      </p>
      <pre className="nt-uischema__props">{JSON.stringify(component.props, null, 2)}</pre>
    </section>
  );
}
