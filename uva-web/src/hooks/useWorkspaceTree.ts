/**
 * The workspace tree's data: the root-level sessions of the single
 * default workspace.
 *
 * Design D35: the default workspace is **always** present — a user can
 * chat without creating one — and this build has no workspace RPC, so
 * the root node is a frontend constant and the children are sessions
 * whose `parent_session_id` is unset (a sub-agent session reaches the
 * user through the delegation card in the transcript, not through the
 * tree).
 *
 * The list refreshes when the window regains focus: a turn finishing
 * in another tab (or the agent's own writes) then show up without a
 * reload.
 */
import { useEffect } from 'react';
import { sessionsApi } from '../api/sessions';
import { useCursorList } from './useCursorList';
import type { SessionSummary } from '../api/types';

export interface WorkspaceTree {
  /** Root-level sessions, newest first. */
  sessions: SessionSummary[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
}

export function useWorkspaceTree(): WorkspaceTree {
  const list = useCursorList<SessionSummary>(
    (cursor, signal) =>
      sessionsApi.list(
        {
          ...(cursor ? { cursor } : {}),
          limit: 50,
          sort: '-created_at',
        },
        signal,
      ),
    'workspace:sessions',
  );
  const { refresh } = list;

  useEffect(() => {
    function onFocus(): void {
      void refresh();
    }
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  }, [refresh]);

  return {
    sessions: list.items.filter((session) => session.parent_session_id === undefined),
    loading: list.loading,
    error: list.error,
    refresh,
  };
}
