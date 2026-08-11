# Interview kit

## Three CV bullets

- Built a Rust analytical SQL engine end to end: hand-written lexer/parser, semantic binder, typed logical and physical plans, custom column store, and profiled batch operators.
- Implemented predicate pushdown, projection pruning, constant folding, statistics-guided join ordering, hash aggregation, and hash/nested-loop joins with explainable plans.
- Reworked execution to retain typed columns and allocation-free numeric join keys; equal-iteration benchmarks measured 7.88× faster pushed filtering, 8.81× faster pruned scans, and 5.76× faster hash join than the pre-refactor commit.

## LinkedIn project description

I built Lamina SQL, a small analytical query engine in Rust, to understand the complete path from SQL text to columnar execution. The core includes a hand-written parser, binder and primitive type system, logical and physical planning, optimizer rules, a custom typed column format, hash aggregation, two join algorithms, explain plans, and per-operator profiling. I then profiled and replaced tagged intermediate cells and formatted join keys with typed vectors and typed hash keys. Equal-iteration raw benchmarks on the checked-in 20k-row workload measured pushed filtering at 0.193 ms versus 1.517 ms before the refactor and hash join at 1.675 ms versus 9.642 ms. It is intentionally an educational single-threaded engine—not a DuckDB competitor—and the repository documents the missing production capabilities.

## 30-second pitch

Lamina SQL is a Rust analytical engine I wrote from the SQL lexer through storage and typed batch execution. A binder rejects ambiguous or ill-typed queries, optimizer rules push filters and prune columns, and the physical planner chooses between nested-loop and hash joins from table statistics. The demo prints every plan stage and actual operator rows and timing. After profiling, I kept intermediates typed and removed formatted numeric join keys; the equal-iteration benchmark shows a 5.76× hash-join speedup over the earlier commit. The limitations are explicit: it is single-threaded, complex expressions remain scalar inside batches, and it is not production-ready.

## Two-minute technical pitch

The project starts with a hand-written lexer and precedence parser that produce a serializable AST. Binding resolves aliases and qualified columns into typed positional expressions, detects ambiguity, and validates aggregate and operator types before execution. The logical tree contains scans, filters, projections, aggregates, joins, sorts, and limits.

The optimizer is deliberately inspectable. It folds literal subtrees, simplifies Boolean filters, splits conjunctions, pushes single-table predicates into scans, discovers the base columns actually required, and swaps join inputs when statistics predict a smaller build side. Because column positions are already bound, a reorder inserts a projection to preserve the original schema. The physical planner selects a hash equi-join for sufficiently large inputs and retains nested loop for tiny or general predicates.

Storage begins with CSV inference and uses typed vectors. A custom `LAM1` file stores typed column sections; row count, min/max, exact cardinality, and null counts feed planning. Execution works in configurable column batches. Grouping and equi-joins use hash tables, while every operator reports estimated versus actual rows, inclusive time, and estimated output allocation.

The benchmark harness varies one design choice at a time and records hardware, OS, toolchain, commit, configuration, raw iterations, median and p95 time, scanned rows per second, result bytes, and generated graphs. The checked-in equal-iteration comparison shows pushed filtering improving from 1.517 ms to 0.193 ms, a projection-pruned scan from 2.574 ms to 0.292 ms, and hash join from 9.642 ms to 1.675 ms. Within the optimized engine, pushdown is 2.74× faster than off and hash join is 106.36× faster than nested loop on this particular many-to-one workload. Batching is only 1.07× faster than tuple granularity here, which is why I do not claim SIMD-like gains. These numbers are local evidence, not a DuckDB comparison. The most important next work would be reusable selection vectors, spill-aware streaming, interleaved benchmark trials, parallel pipelines, and hardware-counter-guided SIMD.
