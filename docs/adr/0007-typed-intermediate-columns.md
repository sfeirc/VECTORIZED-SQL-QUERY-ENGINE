# ADR 0007: Preserve typed columns through execution

**Status:** Accepted — 2026-08-11

## Context

The initial executor expanded every scanned cell into a tagged `Value` and formatted numeric join keys as strings. Profiling showed these representation conversions dominating simple filters, projections, and hash joins.

## Alternatives

- Keep generic values and rely on compiler optimization; this preserves simple code but retains per-cell tags, cloning, and formatted-key allocation.
- Generate a fully specialized operator for every type/expression combination; this maximizes specialization but greatly expands the implementation and testing surface.
- Retain typed columns with specialized common kernels and a scalar fallback; this targets measured hot paths without duplicating the whole executor.

## Decision

Use `ColumnData` for scans and intermediate batches, represent pruned columns as absent typed vectors, bulk-copy direct projections, specialize column/literal comparisons, and use typed hash keys. Complex expressions continue through the general `Value` evaluator.

## Consequences

On equal 11-iteration runs, pushed filtering improved from 1.517 ms to 0.211 ms, projection-pruned scans from 2.574 ms to 0.347 ms, and hash join from 9.642 ms to 1.653 ms. The executor has more type-dispatch code and still materializes operator outputs. Batch size 1,024 is only 1.05× faster than tuple granularity in the optimized workload, so the change supports a typed-column claim—not a SIMD claim.
