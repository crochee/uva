# Eval report — PR description from a diff

**Suite:** `pr-description-from-diff` · **Runs per configuration:** 3 · **Threshold:** every metric must score 1.0

## Task set

| Case | Facts asserted | Diff |
|---|---|---|
| t01-json-flag | --json, CLI | `t01-json-flag` |
| t02-empty-input-panic | empty input, panic | `t02-empty-input-panic` |
| t03-log-env-rename | ELYSPER_LOG, ELYSPER_LOG_LEVEL | `t03-log-env-rename` |

## Assertions

1. `KeywordMetric` — every fact from the diff appears in the output;
2. `sections` — `## Summary`, `## Changes`, `## Testing` are all present;
3. `no_fabricated_number` — every numeric token in the output occurs in the diff;
4. `no_filler` — no `robust`, `various`, `improves performance`, `not run`.

A case passes only when all four metrics score 1.0.

## Limits and stop conditions

| Cap | Value | Stop result |
|---|---|---|
| Cases | 20 | `budget-exhausted (max_cases)` |
| Wall clock | 120 s | `budget-exhausted (max_wall)` |
| Output size | 8000 bytes | `stalled (output over max_output_bytes)` |

The stop path was exercised: `agent error: stop: budget-exhausted (max_cases)`.

## Results

| Configuration | Runs | Passed / total | Pass rate | Tokens (est. output) | Duration |
|---|---|---|---|---|---|
| baseline | 3 | 0/9 | 0% | ~317 | 0.00 s |
| candidate | 3 | 9/9 | 100% | ~483 | 0.00 s |

### Per case (candidate)

| Case | Passes / attempts |
|---|---|
| t01-json-flag | 3/3 |
| t02-empty-input-panic | 3/3 |
| t03-log-env-rename | 3/3 |

### Baseline failure modes

- `t01-json-flag`: failed → keyword_match=0, no_fabricated_number=0, no_filler=0
- `t01-json-flag`: failed → keyword_match=0, no_fabricated_number=0, no_filler=0
- `t01-json-flag`: failed → keyword_match=0, no_fabricated_number=0, no_filler=0

### Candidate failure modes

- none

## Recorded run

```console
LIMIT-STOP: agent error: stop: budget-exhausted (max_cases)
baseline: 0/9 passed in 0.00s, ~317 tokens
candidate: 9/9 passed in 0.00s, ~483 tokens
EVAL-SUITE: OK
```

Run with `cargo run --manifest-path evals/Cargo.toml`.
