# Known limitations

Lamina SQL is single-threaded, in-process, and memory-resident. It has no transactions, concurrency control, recovery, durability guarantees, authentication, network protocol, resource governance, disk spill, or untrusted-input hardening.

SQL support is deliberately incomplete. Null semantics, numeric overflow behavior, collation, identifiers, CSV edge cases, and aggregate expression composition do not cover the SQL standard. Cardinality estimates are heuristics. Statistics are exact to construct and can be expensive. The storage format has no checksums, compression, evolution, or compatibility promise.

Execution uses generic tagged values and materializes intermediate columns. Batch loops improve the recorded workload, but there is no SIMD proof, cache-counter evidence, or parallel speedup claim. Memory fields in profiles are result allocation estimates, not process peak usage.

Benchmarks contain three recorded iterations on one laptop-class CPU and deterministic TPC-H-shaped data. They are regression evidence, not a competitive database study.

