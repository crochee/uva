import { describe, expect, it } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { loadCuratedWebFree, withoutDeclarations } from '../lib/curated.js';
import { checkContract } from '../lib/unifier.js';
import type { Endpoint } from '../lib/types.js';

/** A repository-shaped temporary root with a curated file in it. */
function rootWithCurated(contents: string): string {
  const root = mkdtempSync(join(tmpdir(), 'elysper-curated-'));
  mkdirSync(join(root, 'contract-closure', 'curated'), { recursive: true });
  writeFileSync(
    join(root, 'contract-closure', 'curated', 'web-free-endpoints.json'),
    contents,
  );
  return root;
}

const be: Endpoint[] = [
  { id: 'GET /api/tools', method: 'GET', path: '/api/tools', source: 'backend', source_files: { backend: ['router.rs:1'] } },
];
const fe: Endpoint[] = [];

describe('the curated web-free list', () => {
  it('suppresses a route when the declaration names a reason', () => {
    const root = rootWithCurated(
      JSON.stringify([
        { method: 'GET', path: '/api/tools', notes: 'The web app does not call it.' },
      ]),
    );
    const curated = loadCuratedWebFree(root);
    expect(curated).toHaveLength(1);
    const res = checkContract(be, fe, curated);
    expect(res.backend_only).toHaveLength(0);
  });

  it('suppresses nothing when the file is broken', () => {
    const root = rootWithCurated('{ not json');
    expect(loadCuratedWebFree(root)).toEqual([]);
    // A broken curated file must not leave the endpoints unreported:
    // the check falls back to reporting them.
    const res = checkContract(be, fe, loadCuratedWebFree(root));
    expect(res.backend_only.map((d) => d.path)).toContain('/api/tools');
  });

  it('suppresses nothing when the file is missing', () => {
    const root = mkdtempSync(join(tmpdir(), 'elysper-curated-'));
    expect(loadCuratedWebFree(root)).toEqual([]);
  });

  it('refuses a declaration without a reason', () => {
    const root = rootWithCurated(
      JSON.stringify([{ method: 'GET', path: '/api/tools', notes: '   ' }]),
    );
    expect(loadCuratedWebFree(root)).toEqual([]);
    const res = checkContract(be, fe, loadCuratedWebFree(root));
    expect(res.backend_only.map((d) => d.path)).toContain('/api/tools');
  });
});

describe('the scan artifact', () => {
  it('cannot suppress the check that reads it', () => {
    // An artifact entry that claims to be web-free is stripped of its
    // declaration before the check sees it.
    const artifactEntry: Endpoint = {
      id: 'GET /api/tools',
      method: 'GET',
      path: '/api/tools',
      source: 'backend',
      source_files: { backend: ['router.rs:1'] },
      notes: 'carried over from an earlier scan',
      frontend_consumed: false,
    };
    const res = checkContract(be, fe, withoutDeclarations([artifactEntry]));
    expect(res.backend_only.map((d) => d.path)).toContain('/api/tools');
  });
});
