# Interview kit

## Three CV bullets

- Built a Rust analytical SQL engine end to end: hand-written lexer/parser, semantic binder, typed logical and physical plans, custom column store, and profiled batch operators.
- Implemented predicate pushdown, projection pruning, constant folding, statistics-guided join ordering, hash aggregation, and hash/nested-loop joins with explainable plans.
- Created a reproducible six-experiment benchmark harness; on the checked-in 20k-row workload, hash join measured 4.876 ms versus 134.837 ms nested loop, with raw JSON/CSV and generated SVG retained.

## LinkedIn project description

I built Lamina SQL, a small analytical query engine in Rust, to understand the complete path from SQL text to columnar execution. The core includes a hand-written parser, binder and primitive type system, logical and physical planning, optimizer rules, a custom typed column format, hash aggregation, two join algorithms, explain plans, and per-operator profiling. A deterministic benchmark suite records environment and commit metadata plus every raw observation; on the checked-in 20k-row workload, predicate pushdown reduced median execution from 2.793 ms to 0.666 ms and hash join measured 4.876 ms versus 134.837 ms for nested loop. It is intentionally an educational single-threaded engine—not a DuckDB competitor—and the repository documents the missing production capabilities.

## 30-second pitch

Lamina SQL is a Rust analytical engine I wrote from the SQL lexer through storage and vectorized execution. A binder rejects ambiguous or ill-typed queries, optimizer rules push filters and prune columns, and the physical planner chooses between nested-loop and hash joins from table statistics. The demo prints every plan stage and actual operator rows and timing. I also built a reproducible benchmark matrix; for example, its checked-in join workload shows 4.876 ms for hash join versus 134.837 ms for nested loop. The limitations are explicit: it is single-threaded, uses scalar expressions inside batches, and is not production-ready.

## Two-minute technical pitch

The project starts with a hand-written lexer and precedence parser that produce a serializable AST. Binding resolves aliases and qualified columns into typed positional expressions, detects ambiguity, and validates aggregate and operator types before execution. The logical tree contains scans, filters, projections, aggregates, joins, sorts, and limits.

The optimizer is deliberately inspectable. It folds literal subtrees, simplifies Boolean filters, splits conjunctions, pushes single-table predicates into scans, discovers the base columns actually required, and swaps join inputs when statistics predict a smaller build side. Because column positions are already bound, a reorder inserts a projection to preserve the original schema. The physical planner selects a hash equi-join for sufficiently large inputs and retains nested loop for tiny or general predicates.

Storage begins with CSV inference and uses typed vectors. A custom `LAM1` file stores typed column sections; row count, min/max, exact cardinality, and null counts feed planning. Execution works in configurable column batches. Grouping and equi-joins use hash tables, while every operator reports estimated versus actual rows, inclusive time, and estimated output allocation.

The benchmark harness varies one design choice at a time and records hardware, OS, toolchain, commit, configuration, raw iterations, summaries, and a generated graph. On the checked-in 20k-row synthetic TPC-H-shaped run, pushdown was 0.666 ms versus 2.793 ms off, pruning was 2.253 ms versus 4.693 ms off, and hash join was 4.876 ms versus 134.837 ms nested loop. Those numbers are local evidence, not a DuckDB comparison. The most important next work would be selection vectors, spill-aware streaming, calibrated cardinality estimation, parallel pipelines, and hardware-counter-guided SIMD.

