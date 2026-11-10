/**
 * A row's enable switch wired to the profile surface, plus the
 * transaction receipt.
 *
 * The receipt is part of the control, not a toast: `overridden` and
 * `restart-required` need to stay visible next to the switch that
 * caused them, because the switch itself will bounce back to the
 * disk's truth on the next read.
 */
import type { ProfileRow } from '../api/settings-profile';
import { rowDelegated, rowEnabled } from '../api/settings-profile';
import { EnableToggle } from './EnableToggle';
import type { ProfileSurface } from './useProfileSurface';

export interface ProfileToggleProps {
  surface: ProfileSurface;
  row: ProfileRow;
  /** Override the `locked` presentation (e.g. the built-in default
   *  agent is never flippable). */
  locked?: boolean;
  lockedReason?: string;
  label?: string;
  testId?: string;
}

export function ProfileToggle({
  surface,
  row,
  locked = false,
  lockedReason,
  label,
  testId,
}: ProfileToggleProps) {
  const delegated = rowDelegated(row);
  const feedback = surface.feedback[row.id];
  return (
    <>
      {feedback ? (
        <span
          className={`nt-settings__receipt nt-settings__receipt--${feedback.tone}`}
          data-testid={`${testId ?? row.id}-receipt`}
          title={feedback.text}
        >
          {feedback.text}
        </span>
      ) : null}
      <EnableToggle
        checked={rowEnabled(row)}
        busy={surface.pendingRow === row.id}
        locked={locked || delegated}
        lockedReason={
          delegated && typeof row.enabled === 'object'
            ? `启用位由插件判定（${row.enabled.service}.${row.enabled.check}）`
            : lockedReason
        }
        onChange={(next) => void surface.toggle(row.id, next)}
        label={label ?? `启用 ${row.id}`}
        testId={testId}
      />
    </>
  );
}
