# Auto-documented Makefile:
# http://marmelab.com/blog/2016/02/29/auto-documented-makefile.html
#
# This is a library crate: everything is cargo-native. CI runs the same
# lint/test targets on a linux + macos matrix — no containerized pipeline,
# no cross-compilation, nothing to release except (eventually) crates.io.

.PHONY: help build test fmt fmt-check clippy lint check clean sync-fixtures

CARGO_TEST_FLAGS ?=

help:	## Print available targets
	@awk 'BEGIN {FS = ":.*##"; printf "Targets:\n"} /^[a-zA-Z_-]+:.*##/ {printf "  %-20s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build:	## Build the crate (debug)
	cargo build

test:	## Run all tests
	cargo test $(CARGO_TEST_FLAGS)

fmt:	## Format all sources
	cargo fmt --all

fmt-check:	## Verify formatting without modifying
	cargo fmt --all -- --check

clippy:	## Run clippy with deny warnings
	cargo clippy --all-targets -- -D warnings

lint: fmt-check clippy	## Run all lint checks (fmt + clippy)

check: build lint test	## Build + lint + test

clean:	## Remove build artifacts
	cargo clean

sync-fixtures:	## Refresh the vendored envelope fixture corpus from a local tapes checkout
	scripts/sync-envelope-fixtures.sh
