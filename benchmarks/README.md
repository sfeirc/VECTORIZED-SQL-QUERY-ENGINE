# Reproducible benchmarks

The harness implements six controlled experiments over a deterministic TPC-H-shaped customer/orders/lineitem schema. It is not the official TPC-H generator, does not use TPC-H scale factors, and does not produce a compliant score.

## Recorded run

```bash
cargo run --release --bin benchmark -- \
  --rows 20000 --iterations 3 \
  --out benchmarks/results/2026-08-11-windows
```

Run metadata is in `raw.json`: Windows 10.0.26200.8893, x86-64 Intel Family 6 Model 140, 8 logical CPUs, Rust 1.93.1, commit `7d76f660eefbcd717a879497e9098178f176f82f`, release profile with thin LTO. Two unrecorded warm-ups precede three measured iterations.

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
- `results.svg`: generated median bar chart.

The benchmark binary writes all four artifacts; the figure is never edited by hand. Background load, CPU frequency scaling, allocator state, and the small three-sample count limit inference. Re-run on your target machine before using any ratio.

