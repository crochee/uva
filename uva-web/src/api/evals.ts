/**
 * Evals domain client — `uva-eval` surface.
 *
 * Wire shapes follow `crates/uva-eval/src/lib.rs` /
 * `metrics.rs`: `EvalSuite` carries `TestCase`s; `EvalReport` is
 * the runner's structured output with per-case metric scores. The
 * five methods are registered by
 * `crates/uva-app-server/src/rpc/methods/automation.rs`.
 */

import { rpc } from './rpc';
import type { EvalReport, EvalSuite, List } from './types';

const METHOD = {
  list: 'evals/list',
  get: 'evals/get',
  create: 'evals/create',
  remove: 'evals/delete',
  run: 'evals/run',
} as const;

export const evalsApi = {
  /** `evals/list` — stored eval suites. */
  list: (signal?: AbortSignal): Promise<List<EvalSuite>> =>
    rpc.request<List<EvalSuite>>(METHOD.list, undefined, signal),

  /** `evals/get` — single suite detail (cases included). */
  get: (name: string, signal?: AbortSignal): Promise<EvalSuite> =>
    rpc.request<EvalSuite>(METHOD.get, { name }, signal),

  /** `evals/create` — register a suite from JSON; the body is the suite. */
  create: (req: EvalSuite, signal?: AbortSignal): Promise<EvalSuite> =>
    rpc.request<EvalSuite>(METHOD.create, req, signal),

  /** `evals/delete` — drop a stored suite. */
  remove: (name: string, signal?: AbortSignal): Promise<void> =>
    rpc.request<void>(METHOD.remove, { name }, signal),

  /** `evals/run` — run the suite and return the report. */
  run: (name: string, signal?: AbortSignal): Promise<EvalReport> =>
    rpc.request<EvalReport>(METHOD.run, { name }, signal),
};
