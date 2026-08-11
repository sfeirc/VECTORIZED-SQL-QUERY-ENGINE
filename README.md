# Lamina SQL

**A vectorized analytical SQL engine written from scratch in Rust, implementing parsing, logical/physical planning, predicate pushdown, columnar execution and hash joins, benchmarked on TPC-H-shaped workloads.**

```text
SQL  SELECT c.region, SUM(o.total) AS revenue ...

└── Limit 3
    └── Sort [revenue]
        └── HashAggregate [groups=1]
            └── HashJoin on=(c.customer_id = o.customer_id)
                ├── ColumnScan customers [columns=2/3]
                └── ColumnScan orders [columns=2/3, pushed_filters=1]

HashJoin [est=20, in=144, out=140, 0.569 ms, 62.5%, 4,150 bytes]
```

![Measured benchmark medians](benchmarks/results/2026-08-11-windows/results.svg)

The figure contains measured medians from three release-build iterations on the machine recorded in [raw.json](benchmarks/results/2026-08-11-windows/raw.json). The 20,000-row generator is deterministic and TPC-H-shaped, but **is not official `dbgen` output and is not a TPC-H-compliant result**.

## Measured results

| Experiment | Compared variants | Median execution time | Observed ratio |
|---|---|---:|---:|
| Row vs column scan | row / column | 0.159 / 0.061 ms | column 2.58× faster |
| Predicate pushdown | off / on | 2.793 / 0.666 ms | on 4.19× faster |
| Projection pruning | off / on | 4.693 / 2.253 ms | on 2.08× faster |
| Join algorithm | nested loop / hash | 134.837 / 4.876 ms | hash 27.66× faster |
| Execution model | tuple / batches of 1,024 | 1.478 / 0.935 ms | batches 1.58× faster |
| Batch size | 1 / best observed (4,096) | 1.694 / 0.919 ms | 4,096 was 1.84× faster |

These ratios apply only to the checked-in configuration and machine. CPU utilization was not sampled; the harness records wall time and estimated result allocation. “Vectorized” means column-oriented batch processing, not SIMD. Read the [benchmark methodology](benchmarks/README.md) before interpreting the figures.

## Architecture

```mermaid
flowchart LR
    SQL["SQL text"] --> L["Hand-written lexer"]
    L --> P["Precedence parser"]
    P --> A["Typed AST"]
    A --> B["Binder + type checker"]
    B --> LP["Logical plan"]
    LP --> O["Rule optimizer + statistics"]
    O --> PP["Physical planner"]
    PP --> E["Columnar batch operators"]
    S["CSV / LAM1 column store"] --> B
    S --> E
    E --> R["Results + operator profiles"]
```

The core does not embed SQLite or DuckDB. `rusqlite` is a development-only dependency used by one differential test as an external semantics oracle. Runtime dependencies are `serde` and `serde_json`; their use is visible in `Cargo.toml`.

## Quick start

Requires stable Rust (verified with Rust 1.93.1).

```bash
make demo
```

The demo needs no data download or configuration. It constructs two typed tables, prints the SQL, original and optimized logical plans, optimizer actions, physical plan, result table, and operator profile.

Query a CSV directly:

```bash
cargo run --release -- query --csv lineitem=data/lineitem.csv \
  "SELECT l_returnflag, SUM(l_extendedprice) AS revenue FROM lineitem GROUP BY l_returnflag ORDER BY revenue DESC"
```

Inspect plans and ASTs:

```bash
cargo run --release -- query --csv t=data.csv "EXPLAIN SELECT a FROM t WHERE b > 10"
cargo run --release -- query --csv t=data.csv "EXPLAIN PHYSICAL SELECT a FROM t WHERE b > 10"
cargo run -- ast "SELECT a + 1 AS next FROM t"
```

Convert CSV to the custom columnar format and query it:

```bash
cargo run --release -- import data.csv data.lam data
cargo run --release -- query --columnar data=data.lam "SELECT COUNT(*) FROM data"
```

## SQL surface

Implemented:

- `SELECT`, `FROM`, `WHERE`, `GROUP BY`, `ORDER BY`, `LIMIT`;
- `INNER JOIN ... ON` with hash and nested-loop implementations;
- `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`;
- qualified/unqualified columns, table aliases, output aliases, arithmetic, comparisons, `AND`/`OR`/`NOT`;
- `EXPLAIN`, `EXPLAIN PHYSICAL`, and JSON AST output;
- `INT64`, `FLOAT64`, `UTF8`, `BOOLEAN`, and null values.

Unsupported constructs fail during lexing, parsing, or binding rather than during operator execution. The subset excludes DDL/DML, subqueries, outer joins, `HAVING`, window functions, casts, dates/decimals, and full SQL three-valued Boolean semantics.

## Implementation details

The lexer and precedence parser are hand-written. The binder resolves tables, aliases, qualified columns, ambiguous names, aggregate scope, and primitive operator types before execution. Logical operators are `Scan`, `Filter`, `Projection`, `Aggregate`, `Join`, `Sort`, and `Limit`.

The optimizer performs:

- constant folding and simple Boolean-filter elimination;
- conjunctive predicate pushdown into scans and individual join inputs;
- projection pruning, leaving unused persisted columns unread;
- statistics-driven inner-join input reordering while preserving output order;
- physical hash/nested-loop selection from equality shape and estimated input size.

Execution materializes column vectors in configurable batches. Filters create selection indices, projections evaluate a column at a time per batch, aggregation uses a hash table keyed by group values, and equi-joins build a hash table on one input. Every operator records estimated rows, rows in/out, inclusive elapsed time, and estimated output bytes.

`LAM1` is deliberately small: a magic/version marker, table and field metadata, typed column sections, and per-value null markers. Table statistics include row count, min/max, exact in-memory cardinality, and null count. It is not a durable transactional format.

## Why this is difficult

The interesting work is maintaining semantic and positional invariants across stages. A pushed predicate must be remapped when it crosses a join boundary; pruning must retain columns needed only by a downstream join key; join reordering must preserve the query’s output schema; aggregate expressions need separate update/finalize state; and profiles must remain attached to the selected physical operators. Tests target those boundaries rather than only checking the parser in isolation.

## Tests and quality gates

```bash
make check
```

The current suite contains 26 tests covering lexer/parser behavior, binding and type failures, plans and optimizer rules, storage round trips, both join algorithms, execution, tuple/batch equivalence, generated property cases, and a differential aggregate query against SQLite. The SQLite dependency is compiled only for tests.

GitHub Actions runs formatting, Clippy with warnings denied, all-target tests, a release build, and RustSec dependency auditing. Dependabot covers Cargo crates and Actions. The workflow is checked in; this local repository has no GitHub remote, so no hosted Actions run is claimed.

## Reproducing benchmarks

```bash
cargo run --release --bin benchmark -- \
  --rows 20000 --iterations 3 \
  --out benchmarks/results/my-run
```

Each run writes environment/software/commit/configuration metadata, every raw observation in JSON and CSV, median summaries, and an SVG generated from those summaries. See [benchmarks/README.md](benchmarks/README.md) and the [technical report](docs/technical-report.md).

## Engineering decisions

Architectural choices and rejected alternatives live in [docs/adr](docs/adr): hand-written SQL front end, typed columns, batch execution, rule optimization, dual join algorithms, and the `LAM1` persistence format.

## Known limitations

- This is a single-process, single-threaded educational engine, not production-ready software.
- Expressions are scalar inside columnar batches; there is no SIMD, JIT, morsel scheduling, or parallel pipeline execution.
- Intermediate operators materialize complete column sets; there is no spill-to-disk or memory budget.
- Cardinality estimation uses deliberately crude heuristics (commonly 10% filter selectivity) plus base-table statistics.
- Hash keys use a normalized string representation to align integer/float equality; this is correct for tested values but allocation-heavy.
- Null handling is partial and does not implement every SQL three-valued-logic edge case.
- CSV inference is whole-file and the parser supports quoted fields, but not embedded newlines.
- `LAM1` has no checksums, compression, schema evolution, or cross-endian guarantee.
- The benchmark is small, synthetic, single-machine, and excludes DuckDB. No competitiveness claim is made.

## Future work

The next technically meaningful steps are selection vectors without intermediate copies, zone-map pruning from min/max statistics, streaming operators with memory accounting, a cost model calibrated from measurements, parallel pipelines, decimal/date types, and official `dbgen` datasets. SIMD should only be added with before/after counters and benchmark evidence.

## Interview material

CV bullets, a LinkedIn description, and 30-second/two-minute pitches are in [docs/interview-kit.md](docs/interview-kit.md). They use only the checked-in measurements.
