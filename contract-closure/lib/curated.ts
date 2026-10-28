/**
 * Curated contract entries that the scanners cannot derive.
 *
 * The dual-side scan compares the backend router with the web client.
 * Some backend routes exist for consumers that are *not* the web app:
 * an Anthropic-compatible client, an orchestrator probe, the
 * attachment store's own API. Those routes have no frontend call to
 * pair with, and calling them "drift" would be a false alarm — so
 * they are listed here, with the reason they are web-free, and merged
 * into the contract as entries marked `frontend_consumed: false`.
 *
 * The list is reviewed data, not scan output: a re-scan reads it, and
 * the generated `contract.yaml` carries the flag and the note.
 *
 * Two properties are enforced here, not by convention:
 *
 * - a **reason is required**. An entry without a non-empty `notes`
 *   cannot silence anything — a suppression without a reason is
 *   exactly the kind of quiet drift the check exists to catch;
 * - a **broken file suppresses nothing**. A missing or malformed
 *   curated file yields an empty list, so the check reports the
 *   endpoints again instead of inheriting stale suppressions.
 */
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Endpoint, HttpMethod } from './types.js';

interface CuratedWebFree {
  method: HttpMethod;
  path: string;
  notes: string;
}

/** Where the curated list lives, relative to the repository root. */
export const CURATED_FILE = 'contract-closure/curated/web-free-endpoints.json';

/**
 * Load the curated web-free endpoints.
 *
 * A missing, malformed, or reason-less entry yields nothing: the
 * curated data is an annotation, and a broken annotation must not
 * take the scan down with it — the check then reports the endpoints
 * again, which is the honest outcome.
 */
export function loadCuratedWebFree(root: string): Endpoint[] {
  const file = resolve(root, CURATED_FILE);
  if (!existsSync(file)) return [];
  let parsed: CuratedWebFree[];
  try {
    parsed = JSON.parse(readFileSync(file, 'utf8')) as CuratedWebFree[];
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  return parsed
    .filter(
      (entry) =>
        typeof entry?.method === 'string' &&
        typeof entry?.path === 'string' &&
        entry.path.startsWith('/') &&
        // A suppression without a reason is not a declaration.
        typeof entry?.notes === 'string' &&
        entry.notes.trim().length > 0,
    )
    .map((entry) => ({
      id: `${entry.method} ${entry.path}`,
      method: entry.method,
      path: entry.path,
      // The route is a backend route; the web client is not its
      // consumer, which is what `frontend_consumed: false` says.
      source: 'backend',
      source_files: {},
      notes: entry.notes.trim(),
      frontend_consumed: false,
    }));
}

/**
 * Strip the declaration fields from entries that came from a scan
 * artifact.
 *
 * The artifact is *output*, not a declaration: it must never be able
 * to silence the check that reads it. The suppression flag and the
 * reason come from the curated file alone; a re-scan re-applies them
 * to the freshly scanned entries.
 */
export function withoutDeclarations(entries: Endpoint[]): Endpoint[] {
  return entries.map((entry) => {
    const { frontend_consumed: _flag, notes: _notes, ...rest } = entry;
    return rest;
  });
}
