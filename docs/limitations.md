# Known limitations

Lamina SQL is single-threaded, in-process, and memory-resident. It has no transactions, concurrency control, recovery, durability guarantees, authentication, network protocol, resource governance, disk spill, or untrusted-input hardening.

SQL support is deliberately incomplete. Null semantics, numeric overflow behavior, collation, identifiers, CSV edge cases, and aggregate expression composition do not cover the SQL standard. Cardinality estimates are heuristics. Statistics are exact to construct and can be expensive. The storage format has no checksums, compression, evolution, or compatibility promise.

Execution retains typed intermediate columns but still materializes complete operator results. Complex expressions use generic tagged values; there is no SIMD proof, cache-counter evidence, or parallel speedup claim. Memory fields in profiles are result allocation estimates, not process peak usage. On the optimized 20,000-row workload, 1,024-row batches measured only 1.07× faster than tuple granularity.

The current before/after benchmarks contain eleven recorded iterations per commit on one laptop-class CPU and deterministic TPC-H-shaped data. Trials were sequential rather than interleaved. They are regression evidence, not a competitive database study.
