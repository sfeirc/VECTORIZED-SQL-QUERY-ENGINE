# ADR 0006: Minimal LAM1 columnar persistence

**Status:** Accepted — 2026-08-11

## Context

CSV proves ingestion but not ownership of a typed storage representation.

## Alternatives

- Parquet supplies mature encoding, compression, and interoperability but would delegate the core storage exercise.
- JSON is easy to inspect but does not preserve contiguous typed column sections.

## Decision

Write a small binary format with a `LAM1` marker, schema, row count, and typed column sections with null markers. Recompute statistics after load.

## Consequences

The format is simple enough to explain and round-trip test. It has no compression, checksums, indexes, schema evolution, or durability guarantee and should not be used as an interchange standard.

