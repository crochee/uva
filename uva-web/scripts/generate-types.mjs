#!/usr/bin/env node
/**
 * Regenerate `src/api/types.ts` from the server's wire types.
 *
 * The chain is one-way: `crates/uva-app-server/src/api/contract.rs`
 * catalogues every named type the browser reads, `examples/emit-contract.rs`
 * prints that catalog as JSON Schema draft-07, and this script compiles it
 * with `json-schema-to-typescript`. The committed TypeScript is therefore a
 * projection of the Rust types, not a second copy of them.
 *
 * Usage:
 *
 *   npm run generate:types                          # run the Rust emitter, then compile
 *   node scripts/generate-types.mjs --out <path>     # write somewhere else
 *   node scripts/generate-types.mjs --schema <path>  # compile a saved document
 *
 * `--out` is what `scripts/check-web-contract.sh` uses: it regenerates the
 * contract into a scratch file and diffs that against the committed one, so
 * the gate never mutates the tree.
 *
 * The output is deterministic: the emitter sorts its keys and carries no
 * timestamps or paths, and this script formats through the repo's prettier
 * config, so two runs over the same sources are byte-identical. That is
 * what `scripts/check-web-contract.sh` compares.
 */
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { compile } from 'json-schema-to-typescript';
import { format as prettierFormat } from 'prettier';

const HERE = dirname(fileURLToPath(import.meta.url));
const WEB_ROOT = resolve(HERE, '..');
const REPO_ROOT = resolve(WEB_ROOT, '..');
const OUT_FILE = resolve(WEB_ROOT, 'src/api/types.ts');

/** The name of the synthetic root the compiler needs to see a schema. */
const ROOT = 'UvaWebContract';

/** The compiler options the generated file is produced with. */
const OPTIONS = {
  bannerComment: '',
  // The Rust side maps `serde_json::Value` to the unconstrained schema
  // (`true`). `unknown` is the frontend's contract for those fields —
  // `any` would silently accept every misuse.
  unknownAny: true,
  // A Rust struct with named fields is a closed shape; only the maps
  // (`frontmatter`, `scores`, …) opt into an index signature.
  additionalProperties: false,
  // Every definition is a named export, whether or not the synthetic
  // root references it.
  unreachableDefinitions: true,
  // Formatting happens through the repo's prettier config below; the
  // compiler's own formatter would use its defaults.
  format: false,
};

const BANNER = `/**
 * Generated file — do not edit.
 *
 * Every interface below is compiled from the server's wire types:
 * \`crates/uva-app-server/src/api/contract.rs\` catalogues each named
 * response (and request) type with the RPC method that answers it, and
 * \`examples/emit-contract.rs\` prints that catalog as JSON Schema.
 *
 * Regenerate with \`npm run generate:types\` — \`make check-web-contract\`
 * fails when this file and the Rust types disagree.
 */
`;

/** The file to write: the committed contract, or `--out`'s path. */
function readOutFile() {
  const flag = process.argv.indexOf('--out');
  if (flag === -1) return OUT_FILE;
  const path = process.argv[flag + 1];
  if (!path) throw new Error('--out needs a path');
  return resolve(process.cwd(), path);
}

/** Read the schema document, either from the emitter or a saved file. */
function readSchema() {
  const flag = process.argv.indexOf('--schema');
  if (flag !== -1) {
    const path = process.argv[flag + 1];
    if (!path) throw new Error('--schema needs a path');
    return JSON.parse(readFileSync(resolve(process.cwd(), path), 'utf8'));
  }
  const emitted = spawnSync(
    'cargo',
    ['run', '-q', '-p', 'uva-app-server', '--example', 'emit-contract'],
    { cwd: REPO_ROOT, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 },
  );
  if (emitted.error) throw emitted.error;
  if (emitted.status !== 0) {
    throw new Error(`emit-contract failed (${emitted.status}):\n${emitted.stderr}`);
  }
  return JSON.parse(emitted.stdout);
}

/** Prettier options the repo formats `src/` with. */
function prettierOptions() {
  const config = JSON.parse(readFileSync(resolve(WEB_ROOT, '.prettierrc'), 'utf8'));
  return { ...config, parser: 'typescript' };
}

/**
 * Keywords that constrain a value. A sub-schema with none of them allows
 * anything and is exactly the unconstrained schema.
 *
 * `schemars` merges a field's doc comment into its schema, so a
 * `serde_json::Value` field arrives as `{ description: "…" }` rather
 * than `true`. Left alone, the compiler classifies that object as a type
 * and emits an index-signature interface — `{ [k: string]: unknown }` —
 * which is *not* what the server's contract says (a `Value` field may be
 * a scalar or an array). Collapsing such a schema to `true` makes the
 * compiler emit `unknown`, the type the frontend reads these fields as.
 */
const CONSTRAINING = [
  '$ref',
  'type',
  'enum',
  'const',
  'allOf',
  'anyOf',
  'oneOf',
  'not',
  'properties',
  'additionalProperties',
  'patternProperties',
  'items',
  'prefixItems',
  'required',
  'contains',
  'if',
  'then',
  'else',
  'propertyNames',
  'dependentSchemas',
  'dependentRequired',
  'format',
];

/** Collapse unconstrained sub-schemas to `true`, depth-first. */
function collapseUnconstrained(schema) {
  if (schema === null || typeof schema !== 'object') return schema;
  if (Array.isArray(schema)) return schema.map(collapseUnconstrained);
  const out = { ...schema };
  for (const key of [
    'not',
    'additionalProperties',
    'propertyNames',
    'contains',
    'if',
    'then',
    'else',
    'items',
  ]) {
    if (key in out) out[key] = collapseUnconstrained(out[key]);
  }
  for (const key of ['allOf', 'anyOf', 'oneOf', 'prefixItems']) {
    if (Array.isArray(out[key])) out[key] = out[key].map(collapseUnconstrained);
  }
  for (const key of [
    'properties',
    'patternProperties',
    '$defs',
    'definitions',
    'dependentSchemas',
  ]) {
    if (out[key] && typeof out[key] === 'object' && !Array.isArray(out[key])) {
      out[key] = Object.fromEntries(
        Object.entries(out[key]).map(([name, sub]) => [name, collapseUnconstrained(sub)]),
      );
    }
  }
  return CONSTRAINING.some((key) => key in out) ? out : true;
}

/**
 * Drop the compiler's per-definition provenance comment.
 *
 * `json-schema-to-typescript` prefixes every definition with
 * "This interface was referenced by `X`'s JSON-Schema via the
 * `definition` …". The provenance is real, but it repeats the definition
 * key on the next line, and the catalog it points at lives in Rust.
 */
function stripDefinitionComments(text) {
  const pattern =
    /\/\*\*\n \* This interface was referenced by [^\n]*\n \* via the `definition` "[^"]+"\.\n \*\/\n/g;
  return text.replace(pattern, '');
}

/** Drop the synthetic root alias the compiler emits from the document's `oneOf`. */
function stripRootAlias(text, title) {
  const start = text.indexOf(`export type ${title} =`);
  if (start === -1) throw new Error(`no \`export type ${title}\` alias to strip`);
  const end = text.indexOf(';', start);
  if (end === -1) throw new Error('unterminated root alias');
  return text.slice(0, start) + text.slice(end + 1);
}

/**
 * Render the one generic interface.
 *
 * JSON Schema has no type parameters, so the emitter describes the list
 * envelope concretely (`List<serde_json::Value>`: `data` is an array of
 * the unconstrained schema) and marks the row `generic: "T"`. Compiling
 * it alone keeps the substitution local; both the declaration and the
 * element type are asserted, so a change in the envelope fails loudly
 * here instead of silently emitting a non-generic `List`.
 */
async function renderGenericEntry(schema, entry) {
  const definition = entry.tsName;
  const document = {
    $schema: schema.$schema,
    oneOf: [{ $ref: `#/definitions/${definition}` }],
    definitions: { [definition]: schema.definitions[definition] },
  };
  const compiled = await compile(document, ROOT, OPTIONS);
  const formatted = await prettierFormat(stripDefinitionComments(compiled), prettierOptions());
  const body = stripRootAlias(formatted, ROOT);
  const declaration = `export interface ${definition} {`;
  const element = 'data: unknown[];';
  if (!body.includes(declaration)) {
    throw new Error(`the generic row did not compile to an interface:\n${body}`);
  }
  if (!body.includes(element)) {
    throw new Error(`the generic row's element array is not the unconstrained schema:\n${body}`);
  }
  return body
    .replace(declaration, `export interface ${entry.tsName}<${entry.generic}> {`)
    .replace(element, `data: ${entry.generic}[];`);
}

async function main() {
  const outFile = readOutFile();
  const schema = readSchema();
  const catalog = schema['x-uva-catalog'];
  if (!Array.isArray(catalog)) throw new Error('the document carries no x-uva-catalog');
  delete schema['x-uva-catalog'];

  for (const name of Object.keys(schema.definitions)) {
    schema.definitions[name] = collapseUnconstrained(schema.definitions[name]);
  }

  const generic = catalog.filter((entry) => entry.generic);
  if (generic.length > 1) throw new Error('more than one generic row');

  let genericSource = '';
  for (const entry of generic) {
    if (!(entry.tsName in schema.definitions)) {
      throw new Error(`${entry.tsName} is catalogued but not defined`);
    }
    genericSource += await renderGenericEntry(schema, entry);
    // The synthetic root must not reference a definition that is gone.
    const pointer = `#/definitions/${entry.tsName}`;
    schema.oneOf = schema.oneOf.filter((ref) => ref.$ref !== pointer);
    delete schema.definitions[entry.tsName];
  }

  const compiled = await compile(schema, ROOT, OPTIONS);
  const formatted = await prettierFormat(stripDefinitionComments(compiled), prettierOptions());
  const body = stripRootAlias(formatted, ROOT);
  const source = await prettierFormat(BANNER + body + genericSource, prettierOptions());
  writeFileSync(outFile, source);
  process.stderr.write(
    `generate-types: ${catalog.length} catalogued types -> ${outFile.replace(`${REPO_ROOT}/`, '')}\n`,
  );
}

await main();
