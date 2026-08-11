# ADR 0004: Inspectable rule optimizer before a cost model

**Status:** Accepted — 2026-08-11

## Context

Pushdown, pruning, folding, and simplification have clear semantic preconditions. A trustworthy cost model would require calibration data and substantially better cardinality estimates.

## Alternatives

- No optimizer would keep execution simple but miss important cross-stage engineering.
- A Cascades-style cost optimizer is extensible but disproportionate to this SQL surface and available statistics.

## Decision

Apply deterministic rules with counters, then use row-count/distinct/min/max statistics and simple selectivity heuristics for join input order and physical selection.

## Consequences

Plans are explainable and every rule can be disabled in benchmarks. Estimates can be materially wrong, rules are order-sensitive, and this is not a full cost-based optimizer.

