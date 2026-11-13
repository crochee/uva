/**
 * One settings row: glyph, name (+ optional badge), description, and a
 * right-hand action area — the prototype's `.srow`.
 *
 * Pure presentation. Callers own the data, the toggle state and the
 * receipt text; this component only lays them out so eight sections
 * cannot drift apart visually.
 */
import type { ReactNode } from 'react';

export interface RowCardProps {
  /** Emoji / text glyph shown in the leading tile. */
  icon?: string;
  label: ReactNode;
  /** Short marker next to the name (`内置` / `默认` / …). */
  badge?: string;
  description?: ReactNode;
  /** Right-aligned controls (a switch, a link, a receipt…). */
  actions?: ReactNode;
  testId?: string;
}

export function RowCard({ icon, label, badge, description, actions, testId }: RowCardProps) {
  return (
    <li className="nt-settings__row" data-testid={testId}>
      {icon ? (
        <span className="nt-settings__row-icon" aria-hidden>
          {icon}
        </span>
      ) : null}
      <div className="nt-settings__row-main">
        <div className="nt-settings__row-name">
          <span className="nt-settings__row-label">{label}</span>
          {badge ? <span className="nt-settings__row-badge">{badge}</span> : null}
        </div>
        {description ? <div className="nt-settings__row-desc">{description}</div> : null}
      </div>
      {actions ? <div className="nt-settings__row-actions">{actions}</div> : null}
    </li>
  );
}
