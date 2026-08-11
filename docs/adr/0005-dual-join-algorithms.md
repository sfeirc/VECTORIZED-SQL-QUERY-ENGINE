# ADR 0005: Nested-loop and hash joins

**Status:** Accepted — 2026-08-11

## Context

The engine needs a general correctness fallback and an analytical equi-join implementation whose benefit can be measured.

## Alternatives

- Nested loop alone handles any predicate but scales with the input cross-product.
- Sort-merge join supports ordered/range workloads but adds sorting and more physical properties.

## Decision

Implement nested loop for general inner predicates and hash join for two-column equality. Auto-selection uses equality shape and estimated cross-product size.

## Consequences

The difference is directly benchmarkable, and forced modes test equivalence. Hash keys currently allocate normalized strings and the operator does not spill; sort-merge remains future work.

