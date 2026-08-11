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
	cargo run --release --bin benchmark -- --rows 50000 --iterations 5 --out benchmarks/results/latest

