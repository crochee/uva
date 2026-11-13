/**
 * A titled card inside a settings section — the prototype's `.card`.
 *
 * Sections compose one or more of these; the card owns the heading,
 * the one-line explanation and the row list container so the eight
 * sections share one rhythm.
 */
import type { ReactNode } from 'react';

export interface SettingsCardProps {
  title: ReactNode;
  description?: ReactNode;
  /** Controls in the header (a reload button, create actions). */
  actions?: ReactNode;
  children?: ReactNode;
  testId?: string;
}

export function SettingsCard({ title, description, actions, children, testId }: SettingsCardProps) {
  return (
    <section className="nt-settings__card" data-testid={testId}>
      <div className="nt-settings__card-head">
        <h2 className="nt-settings__card-title">{title}</h2>
        {actions ? <div className="nt-settings__card-actions">{actions}</div> : null}
      </div>
      {description ? <p className="nt-settings__card-desc">{description}</p> : null}
      {children}
    </section>
  );
}
