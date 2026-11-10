/**
 * A controlled enable switch for a settings row.
 *
 * Three visual states, all honest about why they differ:
 *
 * - `checked` — the row is enabled in the resolved profile;
 * - `checked === false` — disabled;
 * - `locked` — the row cannot be flipped from here (`内置 · 不可停用`
 *   for the default agent, or a row whose enable bit no wire exposes).
 *   The switch stays focusable-disabled with the reason in `title`, so
 *   the control is not silently missing.
 *
 * The caller owns the state: this component never writes anything. A
 * click hands `!checked` back and the receipt (applied / restart-
 * required / overridden / failed) is rendered by the row, not here.
 */
export interface EnableToggleProps {
  checked: boolean;
  onChange?: (next: boolean) => void;
  /** Render disabled with the reason in `title`. */
  locked?: boolean;
  lockedReason?: string;
  /** Disable while a write is in flight without changing the reason. */
  busy?: boolean;
  /** Accessible name; defaults to a generic one so an unlabeled switch
   *  never ships. */
  label: string;
  testId?: string;
}

export function EnableToggle({
  checked,
  onChange,
  locked = false,
  lockedReason = '内置 · 不可停用',
  busy = false,
  label,
  testId,
}: EnableToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      aria-disabled={locked || busy ? true : undefined}
      className={`nt-settings__toggle${checked ? ' nt-settings__toggle--on' : ''}${
        locked ? ' nt-settings__toggle--locked' : ''
      }`}
      disabled={locked || busy}
      onClick={() => onChange?.(!checked)}
      title={locked ? lockedReason : checked ? '已启用' : '已停用'}
      data-testid={testId}
      data-locked={locked ? '' : undefined}
    >
      <span className="nt-settings__toggle-knob" aria-hidden />
    </button>
  );
}
