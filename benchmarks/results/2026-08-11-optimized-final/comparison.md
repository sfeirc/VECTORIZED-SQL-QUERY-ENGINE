# Before/after benchmark comparison

Baseline: `benchmarks/results/2026-08-11-baseline11-bcb1c18/summary.json`  
Candidate: `benchmarks/results/2026-08-11-optimized-final/summary.json`

| Experiment | Variant | Baseline | Candidate | Speedup | Latency change |
|---|---|---:|---:|---:|---:|
| A_storage_layout | column | 0.068 ms | 0.040 ms | 1.72× | -41.7% |
| A_storage_layout | row | 0.090 ms | 0.059 ms | 1.52× | -34.3% |
| B_predicate_pushdown | off | 3.466 ms | 0.528 ms | 6.56× | -84.8% |
| B_predicate_pushdown | on | 1.517 ms | 0.193 ms | 7.88× | -87.3% |
| C_projection_pruning | off | 8.061 ms | 1.776 ms | 4.54× | -78.0% |
| C_projection_pruning | on | 2.574 ms | 0.292 ms | 8.81× | -88.6% |
| D_join_algorithm | hash | 9.642 ms | 1.675 ms | 5.76× | -82.6% |
| D_join_algorithm | nested_loop | 184.601 ms | 178.135 ms | 1.04× | -3.5% |
| E_execution_model | tuple | 3.409 ms | 0.577 ms | 5.91× | -83.1% |
| E_execution_model | vectorized | 2.027 ms | 0.537 ms | 3.78× | -73.5% |
| F_batch_size | 1 | 3.352 ms | 0.579 ms | 5.78× | -82.7% |
| F_batch_size | 1024 | 1.824 ms | 0.554 ms | 3.29× | -69.6% |
| F_batch_size | 16 | 2.529 ms | 0.576 ms | 4.39× | -77.2% |
| F_batch_size | 256 | 2.088 ms | 0.560 ms | 3.73× | -73.2% |
| F_batch_size | 4096 | 1.917 ms | 0.556 ms | 3.45× | -71.0% |
| F_batch_size | 64 | 2.206 ms | 0.559 ms | 3.94× | -74.6% |

Negative latency change is an improvement. Medians are compared only when experiment and variant names match.
