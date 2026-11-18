// The web half of `scripts/check-row-vocabulary.sh`: report every
// string literal in `uva-web/src` that spells a session row dialect.
//
// The server publishes current rows only, so a dialect spelling in
// the browser's source is a reader recognising a shape the wire no
// longer carries — the exact second declaration of the vocabulary
// the Rust gate forbids. The 33 current tags stay legal here: the
// browser's row table is a `Record` over the generated union, and
// `tsc` already forces it to account for every tag.
//
// Usage: node uva-web/scripts/check-row-vocabulary.mjs <spelling...>
// Output: one `<repo-relative path>\t<line>\t<spelling>` per hit.
// Exit 0 always; the bash gate owns the verdict (exemptions, ratchet).
//
// The TypeScript compiler API is the scanner on purpose: an AST walk
// sees code, not bytes, so a dialect name inside a comment or a
// JSX attribute value that is not a string literal is not a hit.

import fs from 'node:fs';
import path from 'node:path';
import ts from 'typescript';
const args = process.argv.slice(2);
// `--root <dir>` overrides the scanned tree (the gate's self-test
// points it at a fixture directory); the default is the real source.
const rootAt = args.indexOf('--root');
const scannedRoot = rootAt === -1 ? null : args[rootAt + 1];
const dialects = rootAt === -1 ? args : args.slice(0, rootAt).concat(args.slice(rootAt + 2));
const set = new Set(dialects);
if (set.size === 0) {
  console.error('usage: check-row-vocabulary.mjs [--root <dir>] <spelling...>');
  process.exit(2);
}
const webRoot = path.resolve(import.meta.dirname, '..');
const repoRoot = path.resolve(webRoot, '..');
const srcRoot = scannedRoot ?? path.join(webRoot, 'src');

/** Every `.ts` / `.tsx` file under the scanned root, sorted. */
function sourceFiles(dir, acc) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      sourceFiles(full, acc);
    } else if (/\.(ts|tsx)$/.test(entry.name)) {
      acc.push(full);
    }
  }
  return acc;
}

const files = sourceFiles(srcRoot, []).sort();
if (files.length === 0) {
  console.error(`no source files under ${srcRoot} — the gate would pass vacuously`);
  process.exit(2);
}

const hits = [];
for (const file of files) {
  const source = ts.createSourceFile(
    file,
    fs.readFileSync(file, 'utf8'),
    ts.ScriptTarget.Latest,
    /* setParentNodes */ true,
  );
  const visit = (node) => {
    // A synthesized node (no source position) is not source code.
    if (node.pos >= 0) {
      if (
        (ts.isStringLiteral(node) || node.kind === ts.SyntaxKind.NoSubstitutionTemplateLiteral) &&
        set.has(node.text)
      ) {
        hits.push(
          `${path.relative(repoRoot, file)}\t${source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1}\t${node.text}`,
        );
      }
    }
    node.forEachChild(visit);
  };
  visit(source);
}
for (const hit of hits) {
  console.log(hit);
}
