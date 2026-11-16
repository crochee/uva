/**
 * Schedules domain client — `uva-scheduler` surface.
 *
 * Wire shape follows `crates/uva-scheduler/src/job.rs::Job`.
 * The list method returns a tight `ScheduleSummary` row; the detail
 * method flattens the full `Job` so every field is at the top level
 * of the response. The six methods are registered by
 * `crates/uva-app-server/src/rpc/methods/automation.rs`.
 */
import { rpc } from './rpc';
import type {
  CreateScheduleRequest,
  List,
  Schedule,
  ScheduleSummary,
  ScheduleTick,
  UpdateScheduleRequest,
} from './types';

const METHOD = {
  list: 'schedules/list',
  get: 'schedules/get',
  create: 'schedules/create',
  patch: 'schedules/patch',
  remove: 'schedules/delete',
  tick: 'schedules/tick',
} as const;

export const schedulesApi = {
  /** `schedules/list` — every scheduled job. */
  list: (signal?: AbortSignal): Promise<List<ScheduleSummary>> =>
    rpc.request<List<ScheduleSummary>>(METHOD.list, undefined, signal),

  /** `schedules/get` — single job detail. */
  get: (id: string, signal?: AbortSignal): Promise<Schedule> =>
    rpc.request<Schedule>(METHOD.get, { id }, signal),

  /** `schedules/create` — register a new schedule. */
  create: (req: CreateScheduleRequest, signal?: AbortSignal): Promise<Schedule> =>
    rpc.request<Schedule>(METHOD.create, req, signal),

  /** `schedules/patch` — pause / resume / update payload. */
  update: (id: string, req: UpdateScheduleRequest, signal?: AbortSignal): Promise<Schedule> =>
    rpc.request<Schedule>(METHOD.patch, { id, ...req }, signal),

  /** `schedules/delete` — remove the job and its on-disk file. */
  remove: (id: string, signal?: AbortSignal): Promise<void> =>
    rpc.request<void>(METHOD.remove, { id }, signal),

  /** `schedules/tick` — drive a single tick and return the fired
   * jobs (debug / "fire now" affordance). */
  tick: (id: string, signal?: AbortSignal): Promise<ScheduleTick> =>
    rpc.request<ScheduleTick>(METHOD.tick, { id }, signal),
};
