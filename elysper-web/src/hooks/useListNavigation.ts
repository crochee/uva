/**
 * Vim-first cursor navigation for a rendered list (design D30).
 *
 * The hook owns a cursor id, not an index: the list can refetch and
 * reorder under the user without the cursor jumping, and a filtered list
 * that no longer contains the id drops the cursor instead of selecting a
 * stranger.
 *
 * Keys (all skipped while a text control has focus and while a dialog
 * is open):
 *
 * | key            | action                                  |
 * | -------------- | --------------------------------------- |
 * | `j` / `k`      | next / previous                         |
 * | `gg` / `G`     | first / last (`gg` needs two within 600ms) |
 * | `Ctrl+d` / `Ctrl+u` | half a page (container height / row) |
 * | `Enter`        | open the cursor item                    |
 * | `x`            | delete the cursor item (caller confirms) |
 * | `y`            | copy the cursor id                      |
 *
 * The consumer marks its rows with `data-cursor="true"`, which is also
 * what the hook scrolls into view after a move.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { isEditableTarget } from './useKeyboardShortcuts';

/** Second `g` must land inside this window to count as `gg`. */
const GG_WINDOW_MS = 600;

/** Fallback row height when the container has not been measured. */
const DEFAULT_ROW_HEIGHT = 28;

export interface ListNavigationOptions {
  itemIds: ReadonlyArray<string>;
  onOpen: (id: string) => void;
  /** `x`; omit to disable deletion for this list. */
  onDelete?: (id: string) => void;
  /** `y`; omit to disable id copying. */
  onCopyId?: (id: string) => void;
  /** Scroll container; used for half-page math and `scrollIntoView`. */
  containerRef?: React.RefObject<HTMLElement | null>;
  /** False when another surface owns the keys (settings vs chat). */
  enabled?: boolean;
}

export interface ListNavigation {
  cursor: string | null;
  setCursor: (id: string | null) => void;
}

function isDialogOpen(): boolean {
  return document.querySelector('[role="dialog"]') !== null;
}

export function useListNavigation({
  itemIds,
  onOpen,
  onDelete,
  onCopyId,
  containerRef,
  enabled = true,
}: ListNavigationOptions): ListNavigation {
  const [cursor, setCursor] = useState<string | null>(null);
  const lastGRef = useRef(0);
  // Join into a string so the effect below sees a value, not a fresh
  // array identity on every render.
  const idsKey = itemIds.join('\n');

  // A list that no longer contains the cursor (filtered, refreshed) has
  // no cursor: selecting the nearest stranger would be worse.
  useEffect(() => {
    if (cursor !== null && !itemIds.includes(cursor)) setCursor(null);
  }, [cursor, idsKey, itemIds]);

  // Keep the cursor row visible. `block: 'nearest'` is what makes
  // half-page jumps feel like they moved one screen, not jumped a lot.
  useEffect(() => {
    if (cursor === null) return;
    const row = containerRef?.current?.querySelector('[data-cursor="true"]');
    row?.scrollIntoView({ block: 'nearest' });
  }, [cursor, containerRef]);

  const moveTo = useCallback(
    (index: number) => {
      if (itemIds.length === 0) return;
      const clamped = Math.min(Math.max(index, 0), itemIds.length - 1);
      setCursor(itemIds[clamped]);
    },
    [itemIds],
  );

  useEffect(() => {
    if (!enabled) return;
    function onKeyDown(event: KeyboardEvent): void {
      // Dialog Esc handling lives in `<Modal>`; while one is open the
      // list behind it must not move.
      if (isDialogOpen()) return;
      if (isEditableTarget(event.target)) return;
      if (event.altKey || event.metaKey) return;
      if (event.defaultPrevented) return;

      const currentIndex = cursor === null ? -1 : itemIds.indexOf(cursor);
      // Half a page is measured against the container, not a guess: the
      // first navigation sets the cursor, so its row is a valid sample
      // once one exists.
      const container = containerRef?.current;
      const measuredRow =
        container?.querySelector<HTMLElement>('[data-node-id]')?.getBoundingClientRect().height ??
        0;
      const rowHeight = measuredRow > 0 ? measuredRow : DEFAULT_ROW_HEIGHT;
      const page = Math.max(1, Math.floor((container?.clientHeight ?? 480) / rowHeight / 2));

      if (event.ctrlKey) {
        if (event.key.toLowerCase() === 'd') {
          event.preventDefault();
          moveTo(currentIndex + page);
        } else if (event.key.toLowerCase() === 'u') {
          event.preventDefault();
          moveTo(currentIndex - page);
        }
        return;
      }

      switch (event.key) {
        case 'j':
          event.preventDefault();
          moveTo(currentIndex + 1);
          break;
        case 'k':
          event.preventDefault();
          moveTo(currentIndex <= 0 ? 0 : currentIndex - 1);
          break;
        case 'G':
          event.preventDefault();
          moveTo(itemIds.length - 1);
          break;
        case 'g': {
          const now = Date.now();
          if (now - lastGRef.current < GG_WINDOW_MS) {
            event.preventDefault();
            moveTo(0);
            lastGRef.current = 0;
          } else {
            lastGRef.current = now;
          }
          break;
        }
        case 'Enter':
          if (cursor !== null) {
            event.preventDefault();
            onOpen(cursor);
          }
          break;
        case 'x':
          if (cursor !== null && onDelete) {
            event.preventDefault();
            onDelete(cursor);
          }
          break;
        case 'y':
          if (cursor !== null && onCopyId) {
            event.preventDefault();
            onCopyId(cursor);
          }
          break;
        default:
          break;
      }
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [enabled, cursor, itemIds, moveTo, onOpen, onDelete, onCopyId, containerRef]);

  return { cursor, setCursor };
}
