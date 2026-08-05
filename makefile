# Auto-documented Makefile:
# http://marmelab.com/blog/2016/02/29/auto-documented-makefile.html
#
# This repo holds two library crates: everything is cargo-native. CI runs the
# same lint/test targets on a linux + macos matrix — no containerized
# pipeline, no cross-compilation, nothing to release except (eventually)
# crates.io.
#
# The repository root is the `tapes-harnesses` crate; `cassette-client/` is a
# sibling package (`tapes-cassette-client`) that is deliberately its own
# workspace root, so each crate can be consumed by git pin independently.
# Every target below therefore runs cargo twice — once per package — rather
# than relying on a shared workspace that does not exist.

.PHONY: help build test fmt fmt-check clippy lint check clean sync-fixtures

CARGO_TEST_FLAGS ?=

# The sibling package's manifest, threaded through every target so the two
# crates cannot drift out of the gates.
CASSETTE_CLIENT_MANIFEST := cassette-client/Cargo.toml

help:	## Print available targets
	@awk 'BEGIN {FS = ":.*##"; printf "Targets:\n"} /^[a-zA-Z_-]+:.*##/ {printf "  %-20s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build:	## Build both crates (debug)
	cargo build
	cargo build --manifest-path $(CASSETTE_CLIENT_MANIFEST)

test:	## Run all tests in both crates
	cargo test $(CARGO_TEST_FLAGS)
# `envelope-fixtures` is off by default, so the default run above never
# compiles the corpus reader or the consumer-facing tests that prove it is
# usable from outside the crate. Run the feature on too, or the whole point of
# exposing it regresses silently.
	cargo test --all-features $(CARGO_TEST_FLAGS)
	cargo test --manifest-path $(CASSETTE_CLIENT_MANIFEST) $(CARGO_TEST_FLAGS)

fmt:	## Format all sources in both crates
	cargo fmt --all
	cargo fmt --all --manifest-path $(CASSETTE_CLIENT_MANIFEST)

fmt-check:	## Verify formatting without modifying
	cargo fmt --all -- --check
	cargo fmt --all --manifest-path $(CASSETTE_CLIENT_MANIFEST) -- --check

clippy:	## Run clippy with deny warnings on both crates
	cargo clippy --all-targets -- -D warnings
	cargo clippy --all-targets --all-features -- -D warnings
	cargo clippy --all-targets --manifest-path $(CASSETTE_CLIENT_MANIFEST) -- -D warnings

lint: fmt-check clippy	## Run all lint checks (fmt + clippy)

check: build lint test	## Build + lint + test

clean:	## Remove build artifacts from both crates
	cargo clean
	cargo clean --manifest-path $(CASSETTE_CLIENT_MANIFEST)

sync-fixtures:	## Refresh the vendored envelope fixture corpus from a local tapes checkout
	scripts/sync-envelope-fixtures.sh
