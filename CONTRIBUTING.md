# Contributing

Keep changes narrow and evidence-backed. Run `make check` before opening a pull request. New SQL syntax needs parser and binder failure tests; optimizer rules need plan and execution tests; performance claims need raw benchmark artifacts produced by the checked-in harness.

Do not describe the engine as production-ready, delete unfavorable measurements, or add benchmark ratios without preserving configuration and raw observations. Record architecture-changing choices in `docs/adr`.
