# ADR 0002: Hand-written SQL front end

**Status:** Accepted — 2026-08-11

## Context

Parser behavior is part of the learning objective, including precedence and useful byte-positioned errors.

## Alternatives

- `sqlparser-rs` provides far broader, better-tested syntax but would hide the lexer/parser implementation.
- A parser generator makes the grammar declarative but adds generated machinery for a small subset.

## Decision

Implement the tokenizer and Pratt-style expression parser directly. Serialize the AST with `serde` for visualization.

## Consequences

The supported grammar is inspectable and tested. The tradeoff is a narrow dialect without quoted identifiers, subqueries, or full standard conformance; each extension requires explicit grammar work.

