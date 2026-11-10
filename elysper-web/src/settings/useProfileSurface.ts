/**
 * One fetch of `profile/list` plus the transactional write that flips
 * a row's enable bit.
 *
 * Every section shares this hook, which is how the settings surface
 * keeps its single-seam rule (D26): sections never call the wire
 * themselves, they read `rows` and call `toggle`. Two behaviours are
 * load-bearing:
 *
 * - a committed write is broadcast as `profile/changed`, and the
 *   subscription re-reads, so a write from another client (or the
 *   agent's settings MCP) lands here without a reload;
 * - the receipt is kept per row and rendered verbatim — `applied` /
 *   `restart-required` / `overridden` / `failed` are four different
 *   truths about the disk (see `receipt.ts`).
 */
import { useCallback, useEffect, useState } from 'react';
import { settingsProfile, type ProfileRow } from '../api/settings-profile';
import { failureReceipt, receiptOf, type RowFeedback } from './receipt';

export interface ProfileSurface {
  /** `null` until the first read lands. */
  rows: ProfileRow[] | null;
  error: string | null;
  /** The row whose write is in flight, if any. */
  pendingRow: string | null;
  /** Last transaction receipt per row id. */
  feedback: Record<string, RowFeedback | undefined>;
  toggle: (rowId: string, next: boolean) => Promise<void>;
}

export function useProfileSurface(reloadToken: number): ProfileSurface {
  const [rows, setRows] = useState<ProfileRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<Record<string, RowFeedback | undefined>>({});
  const [pendingRow, setPendingRow] = useState<string | null>(null);
  const [version, setVersion] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    settingsProfile
      .list(controller.signal)
      .then((next) => {
        setRows(next);
        setError(null);
      })
      .catch((cause: Error) => {
        if (cause.name === 'AbortError') return;
        setError(cause.message);
      });
    return () => controller.abort();
  }, [reloadToken, version]);

  useEffect(() => settingsProfile.onChanged(() => setVersion((value) => value + 1)), []);

  const toggle = useCallback(async (rowId: string, next: boolean) => {
    setPendingRow(rowId);
    try {
      const change = await settingsProfile.setEnabled(rowId, next);
      setFeedback((prev) => ({ ...prev, [rowId]: receiptOf(change) }));
    } catch (cause) {
      setFeedback((prev) => ({ ...prev, [rowId]: failureReceipt(cause) }));
    } finally {
      setPendingRow(null);
      // The write may have been `overridden` or `failed` (which do not
      // broadcast), so re-read unconditionally: the row must show the
      // disk's truth, not the click's intent.
      setVersion((value) => value + 1);
    }
  }, []);

  return { rows, error, pendingRow, feedback, toggle };
}
