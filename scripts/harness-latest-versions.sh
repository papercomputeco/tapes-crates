#!/usr/bin/env bash
# Ask each watched harness's upstream what its current version is.
#
# Reads harness-versions.json — the record of what the matrix last passed
# against — and, for every entry with a discovery source, resolves the version
# that source is serving today. Prints one JSON array on stdout:
#
#   [
#     {
#       "name": "claude",
#       "watched": true,
#       "kind": "npm",
#       "source": "@anthropic-ai/claude-code",
#       "recorded_version": "2.1.220 (Claude Code)",
#       "recorded_upstream": "2.1.220",
#       "latest": "2.1.229",
#       "newer": true
#     },
#     { "name": "codex-app", "watched": false, ... , "latest": null, "newer": false }
#   ]
#
# Unwatched harnesses are in the output rather than filtered out of it, with
# `watched: false` and the record's own note. A watcher that printed only the
# harnesses it watches would look identical whether it covered four or one, and
# the reason a harness is unwatched is exactly what a reader needs.
#
# # Failure is loud, and never an empty answer
#
# Every way this can go wrong — an unreachable registry, a package that has been
# renamed, a discovery kind this script does not implement — exits non-zero with
# the reason. It never degrades to "nothing is newer", because that is
# indistinguishable from a healthy run and would turn the scheduled watch off
# without anyone noticing.
#
# # Comparison is exact, not semantic
#
# `newer` is `recorded_upstream != latest`, not a semver ordering. The registry's
# `latest` dist-tag is what a fresh install gets, so a version that moves
# backwards — a release pulled, the tag repointed at its predecessor — is a
# change worth running the matrix against, and a comparison that ordered them
# would silently ignore it. The consequence is that this says "different", and
# the pull request it leads to says which direction.
#
# Usage:
#   scripts/harness-latest-versions.sh [path/to/harness-versions.json]

set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
record="${1:-${here}/harness-versions.json}"

if [ ! -f "${record}" ]; then
  echo "error: no version record at ${record}" >&2
  exit 1
fi

for tool in jq curl; do
  command -v "${tool}" >/dev/null 2>&1 || {
    echo "error: ${tool} is required" >&2
    exit 1
  }
done

# The npm registry's dist-tags endpoint: small, unauthenticated, and it answers
# the question directly rather than making us reason about a full packument.
npm_latest() {
  package="$1"
  # A scoped name's slash must be percent-encoded, or the registry reads it as a
  # path segment and answers 404.
  encoded="${package//\//%2f}"
  body="$(curl -fsSL --retry 3 --retry-delay 2 --max-time 30 \
    "https://registry.npmjs.org/-/package/${encoded}/dist-tags")" || {
    echo "error: could not reach the npm registry for ${package}" >&2
    return 1
  }
  latest="$(printf '%s' "${body}" | jq -r '.latest // empty')"
  if [ -z "${latest}" ]; then
    echo "error: ${package} has no 'latest' dist-tag" >&2
    return 1
  fi
  printf '%s' "${latest}"
}

results='[]'
names="$(jq -r '.harnesses[].name' "${record}")"

while IFS= read -r name; do
  [ -n "${name}" ] || continue

  entry="$(jq -c --arg n "${name}" '.harnesses[] | select(.name == $n)' "${record}")"
  kind="$(printf '%s' "${entry}" | jq -r '.discovery.kind')"
  source_name="$(printf '%s' "${entry}" | jq -r '.discovery.source // empty')"

  case "${kind}" in
    npm)
      if [ -z "${source_name}" ]; then
        echo "error: ${name} is watched through npm but names no package" >&2
        exit 1
      fi
      latest="$(npm_latest "${source_name}")"
      watched=true
      ;;
    unwatched)
      latest=""
      watched=false
      ;;
    *)
      # Not a skip. A kind this script cannot resolve means the record claims
      # coverage that does not exist, and saying so is the whole job.
      echo "error: ${name} declares discovery kind '${kind}', which this script does not implement" >&2
      exit 1
      ;;
  esac

  results="$(
    printf '%s' "${results}" | jq \
      --argjson entry "${entry}" \
      --arg latest "${latest}" \
      --argjson watched "${watched}" \
      '. + [{
         name:              $entry.name,
         watched:           $watched,
         kind:              $entry.discovery.kind,
         source:            $entry.discovery.source,
         note:              $entry.discovery.note,
         recorded_version:  $entry.version,
         recorded_upstream: $entry.upstream_version,
         latest:            (if $latest == "" then null else $latest end),
         newer:             ($watched and $latest != "" and $entry.upstream_version != $latest)
       }]'
  )"
done <<EOF
${names}
EOF

printf '%s\n' "${results}" | jq .
