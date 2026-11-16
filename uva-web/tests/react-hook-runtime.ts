/**
 * A minimal stand-in for React's hook runtime, so a hook file can be
 * driven under vitest's node environment, where no renderer exists
 * (see `docs/adr/0001-frontend-test-policy.md`).
 *
 * It implements exactly the four primitives a hook under test uses,
 * with the two React behaviours the `useCursorList` race depends on
 * modelled faithfully:
 *
 * - a state update never re-renders synchronously — the re-render
 *   happens when the test calls `render()`, mirroring React's
 *   deferred commit;
 * - a `useRef` box is only written by the component body itself, so
 *   whatever it mirrors stays one render behind until then.
 *
 * It is not React: no batching windows, no concurrent rendering, no
 * layout effects. A behaviour that needs those must be verified
 * against the running app (AGENTS.md §4.2).
 */

/** What `useEffect` may return. */
type EffectCleanup = void | (() => void);

/** One per-hook-call slot, addressed in call order like React's dispatcher. */
interface Slot {
  state?: unknown;
  hasState?: boolean;
  ref?: { current: unknown };
  fn?: unknown;
  deps?: readonly unknown[];
  cleanup?: () => void;
}

export interface HookRuntime {
  useState: (initial: unknown) => [unknown, (update: unknown) => void];
  useRef: (initial: unknown) => { current: unknown };
  useEffect: (fn: () => EffectCleanup, deps?: readonly unknown[]) => void;
  useCallback: (fn: unknown, deps: readonly unknown[]) => unknown;
  /** Run (or re-run) the hook, commit its effects, return its result. */
  render: <R>(hook: () => R) => R;
}

export function createHookRuntime(): HookRuntime {
  const slots: Slot[] = [];
  // Effects scheduled by the render in progress, run once the hook
  // body has returned — React's commit phase, not its render phase.
  const commits: Array<() => void> = [];
  let cursor = 0;

  // Take the slot at the cursor (advancing it) and report whether
  // `deps` differ from the ones last committed to that slot. The
  // memo and effect primitives must agree on that answer, or a
  // `useCallback` and the `useEffect` watching it would disagree
  // about whether the effect should re-run.
  const nextSlot = (deps?: readonly unknown[]): { slot: Slot; changed: boolean } => {
    const slot = (slots[cursor] ??= {});
    cursor += 1;
    const previous = slot.deps;
    const changed =
      previous === undefined ||
      deps === undefined ||
      previous.length !== deps.length ||
      previous.some((value, i) => !Object.is(value, deps[i]));
    return { slot, changed };
  };

  const useState = (initial: unknown): [unknown, (update: unknown) => void] => {
    const index = cursor;
    const slot = (slots[cursor] ??= {});
    cursor += 1;
    if (!slot.hasState) {
      slot.state = typeof initial === 'function' ? (initial as () => unknown)() : initial;
      slot.hasState = true;
    }
    // React never re-renders synchronously inside a state update;
    // the next `render()` picks the value up.
    const setState = (update: unknown) => {
      const s = slots[index];
      s.state =
        typeof update === 'function' ? (update as (prev: unknown) => unknown)(s.state) : update;
      s.hasState = true;
    };
    return [slot.state, setState];
  };

  const useRef = (initial: unknown): { current: unknown } => {
    const slot = (slots[cursor] ??= {});
    cursor += 1;
    // React reads the initial value on the first render only; later
    // renders get the same box.
    slot.ref ??= { current: initial };
    return slot.ref;
  };

  const useCallback = (fn: unknown, deps: readonly unknown[]): unknown => {
    const { slot, changed } = nextSlot(deps);
    if (changed) {
      slot.fn = fn;
      slot.deps = deps;
    }
    return slot.fn;
  };

  const useEffect = (fn: () => EffectCleanup, deps?: readonly unknown[]): void => {
    const { slot, changed } = nextSlot(deps);
    if (!changed) return;
    commits.push(() => {
      slot.cleanup?.();
      const cleanup = fn();
      slot.cleanup = typeof cleanup === 'function' ? cleanup : undefined;
      slot.deps = deps;
    });
  };

  const render = <R>(hook: () => R): R => {
    cursor = 0;
    commits.length = 0;
    const result = hook();
    for (const commit of commits.splice(0)) commit();
    return result;
  };

  return { useState, useRef, useEffect, useCallback, render };
}
