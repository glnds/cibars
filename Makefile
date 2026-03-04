.PHONY: build test clippy fmt check run install

build:
	cargo build --release

run:
	cargo run -- --aws-profile pixxis-dev --region eu-west-1 --github-repo glnds/attracr

test:
	cargo test

clippy:
	cargo clippy

fmt:
	cargo fmt

check: fmt clippy test

install: build
	cp target/release/cibars /usr/local/bin/cibars
