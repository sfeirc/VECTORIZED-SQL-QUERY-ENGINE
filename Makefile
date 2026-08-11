.PHONY: demo test check benchmark

demo:
	cargo run --release -- demo

test:
	cargo test --all-targets

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test --all-targets
	cargo build --release

benchmark:
	cargo run --release --bin benchmark -- --rows 50000 --iterations 11 --out benchmarks/results/latest

compare:
	cargo run --release --bin compare_benchmarks -- benchmarks/results/2026-08-11-baseline11-bcb1c18/summary.json benchmarks/results/2026-08-11-optimized-final/summary.json benchmarks/results/2026-08-11-optimized-final
