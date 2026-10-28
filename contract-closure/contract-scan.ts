#!/usr/bin/env node
/**
 * Elysper 双侧契约扫描器入口
 *
 * 读取:
 *   - BACKEND_ROUTER (默认: crates/elysper-server/src/server/router.rs)
 *   - FRONTEND_GLOB (默认: elysper-web/src)
 *   - IN_YAML (默认: docs/interface-contract/contract.yaml；用于保留手工
 *     标记的 fix-card `status: closed`，避免每次扫描被 reset)
 *
 * 写出:
 *   - docs/interface-contract/contract.yaml
 *   - docs/interface-contract/contract.json
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { parse as parseYaml, stringify as yamlStringify } from 'yaml';

import { scanBackendRouter } from './lib/backend-scanner.js';
import { scanFrontendDir } from './lib/frontend-scanner.js';
import { loadCuratedWebFree, withoutDeclarations } from './lib/curated.js';
import { relativizePaths } from './lib/paths.js';
import { unionEndpoints } from './lib/unifier.js';
import type { ContractFile, Endpoint } from './lib/types.js';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const BACKEND_ROUTER = resolve(
  ROOT,
  process.env.BACKEND_ROUTER ?? 'crates/elysper-server/src/server/router.rs',
);
const FRONTEND_ROOT = resolve(
  ROOT,
  process.env.FRONTEND_ROOT ?? 'elysper-web/src',
);
const IN_YAML = resolve(
  ROOT,
  process.env.IN_YAML ?? 'docs/interface-contract/contract.yaml',
);
const OUT_YAML = resolve(ROOT, 'docs/interface-contract/contract.yaml');
const OUT_JSON = resolve(ROOT, 'docs/interface-contract/contract.json');

/**
 * Load the previous `contract.yaml` (if present) and return its
 * endpoints. Used by the scanner to preserve any manually-curated
 * entries (typically fix-card endpoints whose routes the scanner
 * cannot see because they are mounted via `nest_service` or proxied
 * through an external SDK).
 *
 * Both the entries themselves AND their `status: closed` markers are
 * preserved across regenerations; the unifier drops the `status`
 * marker if both scanner sides disagree (see `mergeByKey`), and the
 * `preserve` list re-applies it for scanner-known endpoints.
 */
function loadPreviousDoc(path: string): ContractFile | null {
  if (!existsSync(path)) return null;
  let raw: string;
  try {
    raw = readFileSync(path, 'utf8');
  } catch {
    return null;
  }
  const doc = parseYaml(raw) as ContractFile | null;
  if (!doc || !Array.isArray(doc.endpoints)) return null;
  return doc;
}

function loadPreviousEntries(path: string): Endpoint[] {
  return loadPreviousDoc(path)?.endpoints ?? [];
}

/**
 * The contract's content, independent of how it happens to be
 * serialised (an absent list and an empty list are the same fact).
 *
 * The scan compares this signature with the previous file's, so a
 * re-scan that finds the same contract keeps the previous generation
 * stamp and leaves the committed artifact untouched.
 */
function contractSignature(endpoints: Endpoint[]): string {
  return JSON.stringify(
    endpoints.map((endpoint) => ({
      id: endpoint.id,
      method: endpoint.method,
      path: endpoint.path,
      source: endpoint.source,
      status: endpoint.status ?? null,
      notes: endpoint.notes ?? null,
      frontend_consumed: endpoint.frontend_consumed ?? null,
      backend: endpoint.source_files.backend ?? [],
      frontend: endpoint.source_files.frontend ?? [],
      // The SSE spec is part of the contract: a changed event set is
      // a changed contract, and the generation stamp must say so.
      sse_events: endpoint.sse_events ?? null,
    })),
  );
}

function main() {
  const previous = loadPreviousDoc(IN_YAML);
  // The previous artifact contributes the fix-card `status` markers;
  // the suppression flag and the reason come from the curated file
  // alone, so the artifact can never declare itself out of the check.
  const prev = withoutDeclarations(previous?.endpoints ?? []);
  const curated = loadCuratedWebFree(ROOT);
  const be = relativizePaths(scanBackendRouter(BACKEND_ROUTER), ROOT);
  const fe = relativizePaths(scanFrontendDir(FRONTEND_ROOT), ROOT);
  const cf = unionEndpoints(be, fe, [...prev, ...curated]);

  // The artifact is committed, so a re-scan that finds the same
  // contract must not dirty the tree: the generation stamp only moves
  // when the contract itself changed.
  const unchanged =
    previous !== null &&
    contractSignature(previous.endpoints) === contractSignature(cf.endpoints);
  cf.generated_at = unchanged
    ? previous!.generated_at
    : new Date().toISOString();

  writeFileSync(OUT_YAML, yamlStringify(cf), 'utf8');
  writeFileSync(OUT_JSON, JSON.stringify(cf, null, 2), 'utf8');
  console.log(
    `[contract-scan] backend=${be.length} frontend=${fe.length} ` +
      `preserved=${prev.length} total=${cf.endpoints.length} ` +
      `${unchanged ? 'unchanged' : 'updated'} -> ${OUT_YAML}`,
  );
}

main();
