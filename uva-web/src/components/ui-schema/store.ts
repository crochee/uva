/**
 * The single fetch of `ui/schema`.
 *
 * Every slot region reads this snapshot; none of them fetches on its
 * own. That is the fix for the old per-mount panel debt (design Q10):
 * with one panel per page each mount issued its own request, and the
 * slot tree has several regions (sidebar, session detail, the unplaced
 * bucket) on the same page.
 *
 * Pure TypeScript: no React import, so the projection functions are
 * assertable from the page console.
 */

import { catalogApi, type UiComponent, type UiSchema, type UiSlot } from '../../api/catalog';

/** The projection plus its fetch lifecycle. */
export type UiSchemaState =
  | { status: 'idle' | 'loading' | 'error'; slots: never[]; schemas: never[]; error: string | null }
  | { status: 'ready'; slots: UiSlot[]; schemas: UiSchema[]; error: null };

/** The slots this build places; everything else lands in `UnplacedSlots`. */
export const PLACED_SLOTS: readonly string[] = ['chat.sidebar', 'session.detail.extensions'];

const EMPTY: UiSchemaState = { status: 'idle', slots: [], schemas: [], error: null };

let snapshot: UiSchemaState = EMPTY;
const listeners = new Set<() => void>();
let inFlight = false;

function publish(next: UiSchemaState): void {
  snapshot = next;
  for (const listener of listeners) {
    listener();
  }
}

/** The current snapshot. The object changes only when the state does. */
export function getSnapshot(): UiSchemaState {
  return snapshot;
}

/** Listen for snapshot changes; the returned function unsubscribes. */
export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Fetch `ui/schema` once.
 *
 * Idempotent while a request is in flight, so every region mounting in
 * the same commit produces one request. An aborted fetch leaves the
 * snapshot as it was.
 */
export function refresh(signal?: AbortSignal): void {
  if (inFlight) {
    return;
  }
  inFlight = true;
  if (snapshot.status === 'idle') {
    publish({ status: 'loading', slots: [], schemas: [], error: null });
  }
  catalogApi
    .uiSchema(signal)
    .then((response) => {
      inFlight = false;
      publish({ status: 'ready', slots: response.slots, schemas: response.schemas, error: null });
    })
    .catch((cause: Error) => {
      inFlight = false;
      if (cause.name === 'AbortError') {
        return;
      }
      // The tree is secondary on every page: a failed fetch reports
      // itself instead of taking the page down.
      publish({ status: 'error', slots: [], schemas: [], error: cause.message });
    });
}

/** The components delivered into `slot`, in registration order. */
export function componentsInSlot(state: UiSchemaState, slot: string): UiComponent[] {
  if (state.status !== 'ready') {
    return [];
  }
  return state.schemas
    .flatMap((schema) => schema.components)
    .filter((component) => component.slot === slot);
}

/** The components addressed at slots this build does not place. */
export function unplacedComponents(state: UiSchemaState): UiComponent[] {
  if (state.status !== 'ready') {
    return [];
  }
  return state.schemas
    .flatMap((schema) => schema.components)
    .filter((component) => !PLACED_SLOTS.includes(component.slot));
}
