# ADR 0001: Rust for the engine core

**Status:** Accepted — 2026-08-11

## Context

The project needs explicit memory layout, predictable native performance, and enough type safety to keep planning/execution invariants visible.

## Alternatives

- C++ offers comparable control and mature systems tooling, with more manual lifetime discipline.
- Python would shorten implementation time but obscure the cost model and require native extensions for the central execution work.

## Decision

Use stable Rust and model plans and typed values with enums. Runtime dependencies are limited to serialization support.

## Consequences

Advantages are memory safety, exhaustive operator dispatch, and one release binary. Disadvantages are clone-heavy early implementations, longer compile times (including bundled SQLite in tests), and no automatic escape from generic-value overhead.

