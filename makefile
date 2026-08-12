# Auto-documented Makefile:
# http://marmelab.com/blog/2016/02/29/auto-documented-makefile.html
#
# This repo is one cargo workspace: everything is cargo-native. CI runs the
# same lint/test targets on a linux + macos matrix — no containerized
# pipeline, no cross-compilation, nothing to release except (eventually)
# crates.io.
#
# The root manifest is a workspace and nothing else; every package lives under
# `crates/` and is matched by the `crates/*` member glob. That is what lets
# each target below invoke cargo once and still cover the whole repository,
# including crates added after these targets were written. The older shapes —
# one workspace per package threaded through every target by `--manifest-path`,
# then a hand-written `default-members` list — meant a crate was covered only
# where somebody remembered to name it, and one was in fact missing from CI
# entirely while being present here.

.PHONY: help build test fmt fmt-check clippy lint check clean sync-fixtures pin-parity \
	contracts-check

# Consumer manifests for `make pin-parity`. Defaults to the GitHub repos, which
# is what CI compares; override with local checkouts when working in a forest
# grove, where both consumers are already on disk:
#
#     make pin-parity PIN_PARITY_SOURCES="../../platform/paper ../tapesctl"
PIN_PARITY_SOURCES ?=

CARGO_TEST_FLAGS ?=

help:	## Print available targets
	@awk 'BEGIN {FS = ":.*##"; printf "Targets:\n"} /^[a-zA-Z_-]+:.*##/ {printf "  %-20s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build:	## Build every crate (debug)
	cargo build

test:	## Run all tests in every crate
	cargo test $(CARGO_TEST_FLAGS)
# `envelope-fixtures` is off by default, so the default run above never
# compiles the corpus reader or the consumer-facing tests that prove it is
# usable from outside the crate. Run the feature on too, or the whole point of
# exposing it regresses silently.
	cargo test --all-features $(CARGO_TEST_FLAGS)

fmt:	## Format all sources in every crate
	cargo fmt --all

fmt-check:	## Verify formatting without modifying
	cargo fmt --all -- --check

clippy:	## Run clippy with deny warnings on every crate
	cargo clippy --all-targets -- -D warnings
	cargo clippy --all-targets --all-features -- -D warnings

lint: fmt-check clippy	## Run all lint checks (fmt + clippy)

check: build lint test	## Build + lint + test

clean:	## Remove build artifacts from every crate
	cargo clean

sync-fixtures:	## Refresh the vendored envelope fixture corpus from a local tapes checkout
	scripts/sync-envelope-fixtures.sh

pin-parity:	## Assert every consumer pins the same tapes-harnesses revision
	scripts/check-pin-parity.sh $(PIN_PARITY_SOURCES)

contracts-check:	## Verify the vendored tapes read contract against its recorded fingerprint and the pinned release asset
	scripts/contracts-check.sh
