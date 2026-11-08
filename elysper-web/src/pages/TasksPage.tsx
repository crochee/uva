import { Link } from 'react-router-dom';
import { Box, Flex } from '@radix-ui/themes';
import { Button } from '../components/ui/Button';
import { EmptyState } from '../components/ui/EmptyState';
import { SkeletonList } from '../components/ui/SkeletonList';
import { ListToolbar } from '../components/ui/ListToolbar';
import { PageHero, PageShell, StatusPill, CardGrid } from '../components/ui/visual';
import { useCursorList } from '../hooks/useCursorList';
import type { TaskDelegation } from '../api/types';

const TASK_GLYPH = '🧭';
const TASK_HERO_MODIFIER = 'nt-vis-hero--tool';
const TASK_CARD_MODIFIER = 'nt-vis-card--tool';

/**
 * Tasks list page — sub-agent delegation history.
 *
 * The list is the backend's own: `GET /api/v1/tasks` scans the
 * recent sessions server-side and returns the delegations it
 * found, already ordered newest-first. The endpoint is
 * keyset-paginated like the rest of the v1 surface, so the page
 * follows the cursor with a "Load more" button rather than
 * fetching one unbounded list.
 */
export function TasksPage() {
  const {
    items: delegations,
    loading,
    error,
    hasMore,
    loadMore,
  } = useCursorList<TaskDelegation>('/api/v1/tasks');

  const heroPills = (
    <>
      <StatusPill tone="cyan">{`${delegations.length} delegations`}</StatusPill>
    </>
  );

  return (
    <PageShell testId="tasks-page">
      <PageHero
        glyph={TASK_GLYPH}
        heroModifier={TASK_HERO_MODIFIER}
        title="Tasks"
        subtitle="Sub-agent delegation history. The task tool (`elysper-tool-task`) spawns child agents during a chat run; each delegation is recorded in the child's own session log and listed here."
        pills={heroPills}
        meta={
          <div className="nt-vis-meta-cluster">
            <div className="nt-vis-meta-cluster__row">
              <span>Source</span>
              <span>elysper-tool-task · session delegation records</span>
            </div>
            <div className="nt-vis-meta-cluster__row">
              <span>Depth</span>
              <span>root → grandchild (recursion)</span>
            </div>
          </div>
        }
        testId="tasks-hero"
      />

      <ListToolbar
        query=""
        onQueryChange={() => {
          /* client-side filter placeholder; the page loads by cursor */
        }}
        sortDir="asc"
        onSortDirChange={() => {
          /* sort placeholder */
        }}
        searchLabel="Tasks"
        testId="tasks-toolbar"
      />

      {error ? (
        <EmptyState
          icon="⚠️"
          title="Failed to load delegations"
          description={error}
          testId="tasks-error"
        />
      ) : loading && delegations.length === 0 ? (
        <SkeletonList count={4} testId="tasks-skeleton" />
      ) : delegations.length === 0 ? (
        <EmptyState
          icon="🧭"
          title="No sub-agent delegations yet"
          description={
            <>
              Run a chat session that invokes the <code>task</code> tool to see its sub-agent
              delegations here.
            </>
          }
          testId="tasks-empty"
        />
      ) : (
        <CardGrid testId="tasks-grid">
          {delegations.map((task) => (
            <TaskCard key={task.id} task={task} />
          ))}
        </CardGrid>
      )}

      {hasMore && (
        <div style={{ display: 'flex', justifyContent: 'center' }}>
          <Button
            variant="soft"
            onClick={loadMore}
            disabled={loading}
            data-testid="tasks-load-more"
          >
            {loading ? 'Loading...' : 'Load More Delegations'}
          </Button>
        </div>
      )}
    </PageShell>
  );
}

interface TaskCardProps {
  task: TaskDelegation;
}

function TaskCard({ task }: TaskCardProps) {
  const href = `/tasks/${encodeURIComponent(task.id)}`;
  const tone =
    task.status === 'succeeded'
      ? 'green'
      : task.status === 'failed' || task.status === 'aborted'
        ? 'red'
        : 'yellow';
  return (
    <Link
      to={href}
      className={`nt-vis-card ${TASK_CARD_MODIFIER}`}
      data-testid={`task-card-${task.id}`}
    >
      <Flex justify="between" align="start" mb="2">
        <Box>
          <h3 className="nt-vis-card__name" title={task.agent}>
            <code>{task.agent}</code>
          </h3>
          <span className="nt-vis-card__subtitle">depth · {task.parent_depth}</span>
        </Box>
        <StatusPill tone={tone} withDot>
          {task.status}
        </StatusPill>
      </Flex>
      {task.prompt ? (
        <p className="nt-vis-card__desc">
          {task.prompt.length > 140 ? `${task.prompt.slice(0, 140)}…` : task.prompt}
        </p>
      ) : (
        <p className="nt-vis-card__desc" style={{ color: 'var(--text-muted)' }}>
          No prompt recorded.
        </p>
      )}
      <div className="nt-vis-card__footer">
        <div className="nt-vis-card__pills">
          <StatusPill tone="cyan">delegation</StatusPill>
        </div>
        <div className="nt-vis-card__meta">session · {task.parent_session_id.slice(0, 8)}</div>
      </div>
    </Link>
  );
}
