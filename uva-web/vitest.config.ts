import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    // The node environment is the point, not an accident: this suite
    // covers pure functions only (see
    // `docs/adr/0001-frontend-test-policy.md`), so there is no DOM to
    // emulate — and no jsdom to install, whose engines would not
    // accept CI's Node 20 anyway.
    environment: 'node',
    // Only `tests/` holds test files. `src/` stays outside the scan
    // on purpose: `tsconfig.json` includes `src` alone, so a
    // `*.test.ts` under `src/` would break `tsc --noEmit`. The
    // current policy is ADR 0001 / `AGENTS.md` §4.2: pure-function
    // unit tests live here, e2e does not; `check-web` typechecks the
    // suite through `tsconfig.tests.json` as well as `src`.
    include: ['tests/**/*.test.ts'],
    // A run that finds no test file is a failure, not a pass: a suite
    // that stops finding its own tests must go red, or the gate would
    // report green while testing nothing.
    passWithNoTests: false,
  },
});
