.PHONY: build test lint clean

build:
	cargo build

test:
	cargo test

lint:
	cargo clippy --all-targets --all-features -- -D warnings

fmt:
	cargo fmt

clean:
	cargo clean
