# Technical report

## Scope

Lamina SQL demonstrates the complete analytical query path in one inspectable Rust codebase. It accepts a deliberately constrained SQL dialect, binds and types expressions against an in-memory catalog, applies explicit logical rewrites, selects physical operators, and evaluates typed column data in batches. It is not a transactional database and is not presented as production-ready.

## Query processing

The lexer recognizes keywords, identifiers, numeric/string literals, punctuation, comments, and comparison/arithmetic operators while retaining byte positions for errors. A Pratt-style precedence parser creates a serializable AST. The binder converts names into positional typed references, reports missing/ambiguous columns, restricts aggregate scope, and rejects invalid operator type combinations before planning.

Logical planning constructs scan, filter, projection, aggregate, inner join, sort, and limit trees. `EXPLAIN` prints the optimized logical tree. The rule optimizer folds literal expressions, removes true filters, splits conjuncts, pushes single-input predicates through joins into scans, determines referenced base columns, and reorders a join when the right estimated input is smaller. Reordering inserts a projection so downstream positions remain stable.

The physical planner maps aggregates to hash aggregation and chooses nested-loop or hash equi-join. The automatic heuristic uses join shape and estimated cross-product size. `EXPLAIN PHYSICAL` exposes the result.

## Execution models

The Volcano iterator model calls `next()` per tuple and composes operators lazily. It is discussed here but not used as Lamina’s main operator interface. Lamina’s tuple comparison mode emulates its fine granularity with chunk size one; it is not a full iterator implementation.

The implemented model processes typed column arrays in configurable batches. A scan reads referenced columns, pushed predicates select source positions, direct projections bulk-copy typed vectors, aggregation updates hash states, and joins produce typed columnar results. Column/literal comparisons have specialized kernels and numeric hash joins use typed keys without string formatting. More complex expression dispatch remains scalar and returns `Value`; therefore “vectorized” does not mean SIMD.

Column stores keep values of one attribute together, reducing reads when a query references a subset of a wide schema. Row stores colocate all attributes of one entity, which is often favorable for point access. In the optimized recorded scan microbenchmark, reading the relevant column arrays took a 0.068 ms median versus 0.090 ms for row vectors. This workload is too small to attribute the ratio exclusively to cache misses; no hardware performance counters were collected.

## Optimization and estimation

Predicate pushdown reduces materialized rows before later operators. Projection pruning prevents unreferenced persisted columns from being cloned into an execution data set. Constant folding is observable in the optimization report. Join input ordering uses base row counts and heuristic estimates.

Statistics are exact when a table is constructed: row count, null count, min/max, and distinct count. Equality estimates use distinct counts, numeric range predicates interpolate between min/max, group estimates multiply known distinct counts, and equi-joins divide the input product by the larger key cardinality. Unknown expression shapes retain a 10% fallback. On the demo workload, these changes estimate 137 join rows versus 140 actual and 3 groups versus 3 actual; this is one small example, not proof of estimator accuracy generally.

## Join algorithms

Nested-loop join evaluates the predicate for every input pair and supports general inner predicates. Hash join recognizes a two-column equality, builds a typed key-to-row-list table, probes it, and compares typed values to guard against normalized numeric-key collisions. Duplicate keys are retained. On the optimized recorded many-to-one join, hash join measured 1.653 ms versus 183.162 ms for nested loop. That 110.83× ratio is specific to 5,000 orders, 1,000 customers, and this implementation.

## Storage

CSV import infers primitive types across the input, handles standard quotes and doubled quotes, and builds columns. `LAM1` persists a version marker, schema, row count, and typed column sections with null markers. Statistics are recomputed after loading. The format prioritizes inspectability, not durability or compression.

## Profiling

Each operator profile contains its name, estimated rows, rows in/out, inclusive nanoseconds, estimated output bytes, and child profiles. Inclusive child percentages intentionally overlap; they answer “how much wall time elapsed beneath this node,” not exclusive CPU attribution. Peak RSS and CPU utilization are not measured.

## Implemented versus discussed

| Concept | Status |
|---|---|
| lexer, parser, AST, binder, primitive type checking | implemented |
| logical/physical plans and explain output | implemented |
| constant folding, filter simplification, pushdown, pruning | implemented |
| min/max/distinct estimation, join reordering and physical selection | implemented, heuristic |
| typed column storage and custom persistence | implemented |
| typed batched filters, projections, aggregation, joins | implemented |
| tuple-granularity comparison mode | implemented |
| Volcano iterator architecture | discussed only |
| CPU cache behavior | reasoned about; no counters collected |
| SIMD, JIT, parallel pipelines, spilling, compression | not implemented |
| full cost-based optimization | not implemented |
| official TPC-H conformance | not implemented |

## Reference systems

SQLite is used only in a test to compare overlapping aggregate semantics. DuckDB would be the relevant external analytical reference, but no DuckDB comparison is checked in. DuckDB has a mature optimizer, parallel vectorized pipelines, sophisticated storage/compression, spill behavior, broad SQL semantics, years of correctness work, and platform-specific kernels. Lamina’s local microbenchmarks do not narrow that architectural gap.
