/**
 * Repo-relative source paths.
 *
 * The scanners record where an endpoint came from, and the paths they
 * see are absolute (the router and the web client are resolved against
 * the repository root). An absolute path in a committed artifact ties
 * the contract to the machine that generated it — so the recorded
 * evidence is rewritten to a path relative to the repository root.
 */
import type { Endpoint } from './types.js';

function relativizeOne(file: string, root: string): string {
  const prefix = root.endsWith('/') ? root : `${root}/`;
  return file.startsWith(prefix) ? file.slice(prefix.length) : file;
}

/**
 * Rewrite every recorded source file to its repository-relative path.
 */
export function relativizePaths(
  endpoints: Endpoint[],
  root: string,
): Endpoint[] {
  return endpoints.map((endpoint) => ({
    ...endpoint,
    source_files: {
      backend: endpoint.source_files.backend?.map((file) =>
        relativizeOne(file, root),
      ),
      frontend: endpoint.source_files.frontend?.map((file) =>
        relativizeOne(file, root),
      ),
    },
  }));
}
