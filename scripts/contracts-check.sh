#!/usr/bin/env bash
# Verify the vendored tapes read contract is exactly the published bytes.
#
# Two gates, in order:
#
#   1. The vendored file still carries the fingerprint recorded in
#      crates/tapes-read-contract/contracts/PROVENANCE.md — a hand-edit of the vendored
#      document fails here even with no network and no tapes checkout on the
#      machine.
#   2. Fetch the contract asset from the pinned tapes release and byte-diff it
#      against the vendored copy — a vendoring that does not match the
#      published release fails here.
#
# Gate 2 prefers the release asset (the published source of truth):
#
#   https://github.com/papercomputeco/tapes/releases/download/<tag>/tapes-api-<tag>.yaml
#
# The tag defaults to the pin recorded in PROVENANCE.md; override with
# TAPES_CONTRACT_TAG when checking a bump before the docs are updated. While
# the override names a tag other than the recorded pin — a refresh in
# progress — gate 1's mismatch is reported as expected rather than latched as
# a failure, and gate 2 (against the override tag's asset) is the
# authoritative verdict.
#
# Fallback for offline work or development against an unreleased tapes commit:
# set TAPES_REPO=/path/to/tapes to re-emit the contract from that checkout
# (needs its Go toolchain) and diff the emission instead. When the fetch fails
# and no checkout is available, gate 2 is skipped with a notice so gate 1 still
# runs everywhere — except mid-refresh, where a skipped gate 2 is a failure
# because nothing authoritative ran.
#
# Only the read surface is checked here. The ingest contract is vendored by the
# one client whose capture conformance tests read it, and is checked there.

set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
vendored="${here}/crates/tapes-read-contract/contracts"
provenance="${vendored}/PROVENANCE.md"

# The tag the recorded fingerprint belongs to, per PROVENANCE.md.
pinned_tag="$(grep -oE 'Release tag: \*\*v[0-9]+\.[0-9]+\.[0-9]+\*\*' "${provenance}" \
  | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' || true)"

# A TAPES_CONTRACT_TAG naming a different tag than the recorded pin means a
# refresh is in progress: the vendored bytes are (or are about to be) the new
# tag's, while PROVENANCE.md still records the old one.
refresh=0
if [ -n "${TAPES_CONTRACT_TAG:-}" ] && [ "${TAPES_CONTRACT_TAG}" != "${pinned_tag}" ]; then
  refresh=1
  echo "notice: TAPES_CONTRACT_TAG=${TAPES_CONTRACT_TAG} differs from the recorded pin (${pinned_tag:-none});"
  echo "        treating this as a refresh in progress — gate 1 is informational, gate 2 decides"
fi

# Strict mode: a gate that could not read its input is a FAILURE, not a pass.
#
# Gate 2 is the only gate that can be unavailable — it needs either the network
# or a tapes checkout. Skipping it on a laptop with no connectivity is a
# kindness; skipping it in CI means the seal job goes green having verified
# nothing, which is the one outcome a seal must never produce. Automation says
# so by setting CI, and this can be forced either way for a test.
strict="${TAPES_CONTRACT_STRICT:-}"
if [ -z "${strict}" ]; then
  if [ -n "${CI:-}" ]; then strict=1; else strict=0; fi
fi

# Test seams, and honest overrides for a mirror or a vendored checkout. Neither
# is set in normal use; both exist because the fail-closed behaviour above is
# only worth having if something proves it fires.
release_base="${TAPES_RELEASE_BASE:-https://github.com/papercomputeco/tapes/releases/download}"
fallback_repo="${TAPES_FALLBACK_REPO:-${here}/../tapes}"

fail=0

# --- gate 1: recorded fingerprint --------------------------------------------
name=tapes-api.yaml
recorded="$(grep -oE "\`${name}\` sha256 \`[0-9a-f]{64}\`" "${provenance}" \
  | grep -oE '[0-9a-f]{64}')"
actual="$(shasum -a 256 "${vendored}/${name}" | awk '{print $1}')"
if [ "${recorded}" != "${actual}" ]; then
  if [ "${refresh}" = 1 ]; then
    echo "notice: ${name} does not match the fingerprint recorded in PROVENANCE.md" >&2
    echo "  recorded: ${recorded}" >&2
    echo "  actual:   ${actual}" >&2
    echo "  expected during a refresh to ${TAPES_CONTRACT_TAG}; update PROVENANCE.md before landing" >&2
  else
    echo "FAIL: ${name} does not match the fingerprint recorded in PROVENANCE.md" >&2
    echo "  recorded: ${recorded}" >&2
    echo "  actual:   ${actual}" >&2
    fail=1
  fi
else
  echo "ok: ${name} matches its recorded fingerprint"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# --- gate 2 (fallback): re-emit from a tapes checkout ------------------------
# Explicit TAPES_REPO opts into the local-emission path — development against
# a tapes commit that has no release yet.
emit_and_diff() {
  local tapes_repo="$1"

  # The commit the vendored bytes were published from, per PROVENANCE.md.
  local pinned_commit
  pinned_commit="$(grep -oE 'commit \`[0-9a-f]{7,40}\`' "${provenance}" \
    | head -n1 | grep -oE '[0-9a-f]{7,40}' || true)"

  local head_commit
  head_commit="$(git -C "${tapes_repo}" rev-parse HEAD 2>/dev/null || echo unknown)"
  case "${head_commit}" in
    "${pinned_commit}"*) ;;
    *)
      echo "notice: ${tapes_repo} is at ${head_commit}, not the pinned ${pinned_commit};" >&2
      echo "        a diff below may be a pending contract bump rather than corruption" >&2
      ;;
  esac

  (
    cd "${tapes_repo}"
    GOEXPERIMENT=jsonv2 go run ./cli/tapes dev openapi api --docs-root . --out "${tmp}/${name}"
  )

  if ! diff -u "${vendored}/${name}" "${tmp}/${name}"; then
    echo "FAIL: vendored ${name} differs from the emission at ${head_commit}" >&2
    fail=1
  else
    echo "ok: ${name} matches the re-emission"
  fi
}

if [ -n "${TAPES_REPO:-}" ]; then
  if [ ! -f "${TAPES_REPO}/cli/tapes/main.go" ]; then
    echo "FAIL: TAPES_REPO=${TAPES_REPO} is not a tapes checkout" >&2
    exit 1
  fi
  emit_and_diff "${TAPES_REPO}"
  exit "${fail}"
fi

# --- gate 2 (preferred): fetch the pinned release asset ----------------------
tag="${TAPES_CONTRACT_TAG:-${pinned_tag}}"
if [ -z "${tag}" ]; then
  echo "FAIL: could not determine the pinned release tag from PROVENANCE.md (set TAPES_CONTRACT_TAG)" >&2
  exit 1
fi

base="${release_base}/${tag}"
if curl -fsSL --retry 2 -o "${tmp}/${name}" "${base}/tapes-api-${tag}.yaml"; then
  if ! diff -u "${vendored}/${name}" "${tmp}/${name}"; then
    echo "FAIL: vendored ${name} differs from the ${tag} release asset" >&2
    fail=1
  else
    echo "ok: ${name} matches the ${tag} release asset"
  fi
  exit "${fail}"
fi

# Fetch failed (offline, or the tag's asset is missing): fall back to a
# checkout beside this repo when one exists.
if [ -f "${fallback_repo}/cli/tapes/main.go" ]; then
  echo "notice: could not fetch the ${tag} release asset; falling back to re-emission from ${fallback_repo}"
  emit_and_diff "${fallback_repo}"
  exit "${fail}"
fi

# Nothing authoritative ran. Whether that is survivable depends entirely on
# who is asking: a developer offline still got gate 1, while CI asking the
# same question and being told "fine" is the seal reporting on a comparison it
# never made.
if [ "${refresh}" = 1 ]; then
  echo "FAIL: mid-refresh, but the ${tag} release asset could not be fetched and no tapes" >&2
  echo "      checkout exists at ${fallback_repo} (set TAPES_REPO) — nothing authoritative ran" >&2
  fail=1
elif [ "${strict}" = 1 ]; then
  echo "FAIL: the ${tag} release asset could not be fetched and no tapes checkout exists at" >&2
  echo "      ${fallback_repo} (set TAPES_REPO) — nothing authoritative ran, and a seal that" >&2
  echo "      cannot read its input must block rather than report success" >&2
  fail=1
else
  echo "notice: could not fetch the ${tag} release asset and no tapes checkout at ${fallback_repo} (set TAPES_REPO)."
  echo "        gate 2 DID NOT RUN: the vendored bytes were NOT compared against the published"
  echo "        release, and this run says nothing about whether they match. Gate 1 (the recorded"
  echo "        fingerprint) passed, which only proves the file is unchanged since it was vendored."
  echo "        Re-run with connectivity, or set TAPES_REPO, before trusting this as a verdict."
fi

exit "${fail}"
