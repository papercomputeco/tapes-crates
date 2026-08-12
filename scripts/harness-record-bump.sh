#!/usr/bin/env bash
# Move one harness's entry in harness-versions.json to a version the matrix has
# just passed against.
#
# Usage:
#   scripts/harness-record-bump.sh <name> <printed-version> <upstream-version> [record]
#
# Example:
#   scripts/harness-record-bump.sh claude '2.1.229 (Claude Code)' 2.1.229
#
# # Why this is a script and not four lines of `jq` in a workflow
#
# It is the one thing the scheduled drift watch writes, so it is the one thing
# worth being able to run by hand — before a workflow exists, when a watch run
# has to be reproduced, or when a harness needs bumping for a reason no
# discovery source will ever report. A workflow that inlined this would be the
# only place the rewrite was expressed, and the only way to test it would be to
# wait for a schedule.
#
# # What it refuses
#
# Both versions are required and neither may be empty: an entry with a printed
# version and no upstream version is watched by nothing, and one with an
# upstream version and no printed version cannot be compared to a run. An
# unknown harness, or one the record marks unwatched, is an error rather than a
# new entry — a harness joins the record through the reviewed change that adds
# it to the registry, not through automation that noticed a package.
#
# Everything else in the document is left byte-identical, so the pull request
# this produces is a two-line diff a reviewer can read at a glance.

set -euo pipefail

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
  echo "usage: $0 <name> <printed-version> <upstream-version> [record]" >&2
  exit 1
fi

name="$1"
printed="$2"
upstream="$3"

here="$(cd "$(dirname "$0")/.." && pwd)"
record="${4:-${here}/harness-versions.json}"

command -v jq >/dev/null 2>&1 || {
  echo "error: jq is required" >&2
  exit 1
}

if [ ! -f "${record}" ]; then
  echo "error: no version record at ${record}" >&2
  exit 1
fi

if [ -z "${printed}" ] || [ -z "${upstream}" ]; then
  echo "error: both the printed version and the upstream version are required" >&2
  exit 1
fi

entry="$(jq -c --arg n "${name}" '.harnesses[] | select(.name == $n)' "${record}")"
if [ -z "${entry}" ]; then
  echo "error: ${record} has no entry for ${name}" >&2
  exit 1
fi

kind="$(printf '%s' "${entry}" | jq -r '.discovery.kind')"
if [ "${kind}" = "unwatched" ]; then
  echo "error: ${name} is recorded as unwatched; bumping it needs the reviewed change that makes it watchable" >&2
  exit 1
fi

old_printed="$(printf '%s' "${entry}" | jq -r '.version // "none"')"
old_upstream="$(printf '%s' "${entry}" | jq -r '.upstream_version // "none"')"

updated="$(mktemp)"
trap 'rm -f "${updated}"' EXIT

# `--indent 2` matches the committed file, and jq preserves key order, so the
# diff is exactly the two values that changed.
jq --indent 2 \
  --arg n "${name}" \
  --arg printed "${printed}" \
  --arg upstream "${upstream}" \
  '.harnesses |= map(
     if .name == $n
     then .version = $printed | .upstream_version = $upstream
     else . end
   )' "${record}" >"${updated}"

mv "${updated}" "${record}"
trap - EXIT

echo "${name}: ${old_printed} -> ${printed} (upstream ${old_upstream} -> ${upstream})"
