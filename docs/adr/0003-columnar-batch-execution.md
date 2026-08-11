# ADR 0003: Columnar batch execution

**Status:** Accepted — 2026-08-11

## Context

Analytical queries commonly scan a subset of columns and apply the same expression to many values.

## Alternatives

- A Volcano iterator would be simple and streaming but dispatch once per tuple.
- Full vector kernels with SIMD would offer more optimization potential but exceed the initial correctness scope.

## Decision

Store typed columns and execute configurable chunks. Keep a chunk-size-one mode as the measured tuple-granularity baseline.

## Consequences

Projection pruning maps directly to fewer materialized columns, and batches measured faster on the recorded workload. Intermediate results are still fully materialized and expression evaluation is scalar; the project must not claim SIMD.

