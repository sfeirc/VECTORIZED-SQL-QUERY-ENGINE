# Reproducible benchmarks

The harness implements six controlled experiments over a deterministic TPC-H-shaped customer/orders/lineitem schema. It is not the official TPC-H generator, does not use TPC-H scale factors, and does not produce a compliant score.

## Recorded optimized run

```bash
cargo run --release --bin benchmark -- \
  --rows 20000 --iterations 11 \
  --out benchmarks/results/2026-08-11-optimized-final
```

Run metadata is in `raw.json`: Windows 10.0.26200.8893, x86-64 Intel Family 6 Model 140, 8 logical CPUs, Rust 1.93.1, commit `301547685a6e87b1187ed18b8c27672deb5dd7e4`, release profile with thin LTO. Two unrecorded warm-ups precede eleven measured iterations.

The equal-iteration baseline is commit `bcb1c18f175cd8b41c4bebb4188e143d09b9b78b` in `results/2026-08-11-baseline11-bcb1c18`. It was executed from a detached Git worktree so the binary and recorded commit match.

Generate the checked-in before/after table and graph with:

```bash
cargo run --release --bin compare_benchmarks -- \
  benchmarks/results/2026-08-11-baseline11-bcb1c18/summary.json \
  benchmarks/results/2026-08-11-optimized-final/summary.json \
  benchmarks/results/2026-08-11-optimized-final
```

## Experiments

| ID | Variable changed | Fixed work |
|---|---|---|
| A | row vectors vs typed column vectors | quantity filter plus price sum |
| B | predicate pushdown off/on | filter and two-column projection |
| C | projection pruning off/on | read two of six lineitem columns |
| D | forced nested loop/hash join | customer/orders equi-join and filter |
| E | one-row chunks/1,024-row batches | filter plus arithmetic projection |
| F | batches 1, 16, 64, 256, 1,024, 4,096 | same filter/projection |

`execution_ns` is the inclusive root physical-operator time. `end_to_end_ns` additionally includes parsing, binding, optimization, and physical planning. `output_memory_bytes` is an estimate of result values, not peak RSS. CPU utilization was not sampled and is explicitly marked unavailable.

## Artifacts

- `raw.json`: environment, generator, configuration, and all observations;
- `raw.csv`: measurements in analysis-friendly form;
- `summary.json`: per-variant median execution nanoseconds;
- `metrics.json`: sample count, median and p95 time, median scanned rows/second, and median result bytes;
- `results.svg`: generated median bar chart.
- `comparison.json`, `comparison.md`, and `comparison.svg`: matched before/after medians generated from both summaries.

The benchmark and comparison binaries write every artifact; figures are never edited by hand. Background load, CPU frequency scaling, allocator state, and the eleven-sample count still limit inference. The baseline and candidate ran sequentially rather than under an interleaved harness, so time-dependent machine state remains a confounder. Re-run on your target machine before using any ratio.
