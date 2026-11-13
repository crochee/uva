import { Link } from 'react-router-dom';
import { Box, Flex } from '@radix-ui/themes';
import { StatusPill } from '../ui/visual';
import { describeScheduleKind } from '../../lib/describeScheduleKind';
import type { JobKind, JobStatus } from '../../api/types';

const SCHEDULE_CARD_MODIFIER = 'nt-vis-card--tool';

/** The fields the card renders. Both `Schedule` (detail) and
 *  `ScheduleSummary` (list) satisfy it. */
export interface ScheduleRowView {
  id: string;
  name: string;
  description?: string;
  kind: JobKind;
  status: JobStatus;
  next_fire_at: string;
}

export interface ScheduleCardProps {
  schedule: ScheduleRowView;
  /** Where the card links. Omitted in the settings automation tab,
   *  which has no schedule detail route — the card then renders as a
   *  plain block instead of a dead link. */
  href?: string;
}

export function ScheduleCard({ schedule, href }: ScheduleCardProps) {
  const tone =
    schedule.status === 'active' ? 'green' : schedule.status === 'paused' ? 'cyan' : 'neutral';
  const body = (
    <>
      <Flex justify="between" align="start" mb="2">
        <Box>
          <h3 className="nt-vis-card__name" title={schedule.name}>
            <code>{schedule.name}</code>
          </h3>
          <span className="nt-vis-card__subtitle">{describeScheduleKind(schedule)}</span>
        </Box>
        <StatusPill tone={tone} withDot>
          {schedule.status}
        </StatusPill>
      </Flex>
      {schedule.description ? (
        <p className="nt-vis-card__desc">{schedule.description}</p>
      ) : (
        <p className="nt-vis-card__desc" style={{ color: 'var(--text-muted)' }}>
          No description provided.
        </p>
      )}
      <div className="nt-vis-card__footer">
        <div className="nt-vis-card__pills">
          <StatusPill tone="purple">schedule</StatusPill>
        </div>
        <div className="nt-vis-card__meta">next · {schedule.next_fire_at}</div>
      </div>
    </>
  );

  if (!href) {
    return (
      <div
        className={`nt-vis-card ${SCHEDULE_CARD_MODIFIER}`}
        data-testid={`schedule-card-${schedule.id}`}
      >
        {body}
      </div>
    );
  }
  return (
    <Link
      to={href}
      className={`nt-vis-card ${SCHEDULE_CARD_MODIFIER}`}
      data-testid={`schedule-card-${schedule.id}`}
    >
      {body}
    </Link>
  );
}
