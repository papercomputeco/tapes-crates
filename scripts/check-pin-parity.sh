#!/usr/bin/env bash
# check-pin-parity.sh — assert that every consumer of this crate pins the SAME
# tapes-harnesses revision.
#
# This crate exists so `tapesctl start` and `paper start` share one
# implementation of harness knowledge: ingest parity between them is meant to
# be structural, not policed. A git pin is what makes that true, and two
# consumers on different revisions quietly dissolves it — the two CLIs run
# different attribution code while every test in every repo stays green. This
# has already happened: an audit found paper and tapesctl three commits apart,
# which had silently given them different attribution behaviour.
#
# The check lives here, in the crate, because the invariant is the crate's:
# "my consumers agree about which me they use" is not a fact either consumer
# can state alone. Both call this same script rather than each maintaining its
# own comparison.
#
# Usage:
#   ./scripts/check-pin-parity.sh [SOURCE ...]
#
# Each SOURCE names a consumer manifest, as either:
#   * a path to a Cargo.toml, or to a directory holding one; or
#   * owner/repo[@ref] — fetched with `gh api`, so it works for private
#     repositories when the token in the environment can read them.
#
# With no SOURCE, the defaults below are used.
#
# Exits 0 when every source pins the same revision, 1 when they diverge, and
# 2 when a source cannot be read. A source that cannot be read is an ERROR,
# never a skip: a check that silently passes when it could not look is worse
# than no check, because it reports parity it never verified.

set -euo pipefail

# The consumers whose pins must agree. Add a consumer here when it starts
# depending on this crate.
DEFAULT_SOURCES=(
    papercomputeco/paper
    papercomputeco/tapesctl
)

# The dependency whose `rev` is compared.
CRATE_NAME="tapes-harnesses"

usage() {
    # Reprint this file's leading comment block as the help text: everything
    # from line 2 until the first non-comment line. Derived rather than
    # duplicated so help can't drift from the docs.
    awk 'NR > 1 { if (!/^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
    exit "${1:-0}"
}

case "${1:-}" in
    -h | --help | help)
        usage 0
        ;;
esac

die() {
    echo "check-pin-parity: $*" >&2
    exit 2
}

# Print a source's Cargo.toml on stdout.
read_manifest() {
    local source="$1" path

    # owner/repo[@ref] — exactly one slash, no path separators around it, so a
    # mistyped or missing path is reported as a missing path rather than being
    # hopefully attempted as a GitHub repository.
    if [[ ! -e "$source" && "$source" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(@[A-Za-z0-9_./-]+)?$ ]]; then
        local repo="${source%%@*}" ref="main"
        [[ "$source" == *@* ]] && ref="${source##*@}"
        command -v gh >/dev/null 2>&1 ||
            die "'$source' needs the gh CLI to fetch, and gh is not installed"
        gh api "repos/$repo/contents/Cargo.toml?ref=$ref" \
            --jq '.content' 2>/dev/null | base64 -d 2>/dev/null ||
            die "could not read Cargo.toml from $repo@$ref (is the token authorized for it?)"
        return
    fi

    path="$source"
    [[ -d "$path" ]] && path="$path/Cargo.toml"
    [[ -f "$path" ]] || die "no manifest at '$path'"
    cat "$path"
}

# Extract the pinned revision of $CRATE_NAME from a manifest on stdin.
#
# Matches the dependency line and pulls the `rev = "..."` out of it. Anchored
# on the crate name at the start of a line so a comment mentioning the crate —
# and both consumers' manifests have several — cannot be mistaken for the pin.
extract_rev() {
    awk -v crate="$CRATE_NAME" '
        $0 ~ "^" crate "[[:space:]]*=" {
            if (match($0, /rev[[:space:]]*=[[:space:]]*"[0-9a-fA-F]+"/)) {
                line = substr($0, RSTART, RLENGTH)
                match(line, /[0-9a-fA-F]+"$/)
                print substr(line, RSTART, RLENGTH - 1)
                exit
            }
        }
    '
}

sources=("$@")
[[ ${#sources[@]} -eq 0 ]] && sources=("${DEFAULT_SOURCES[@]}")
[[ ${#sources[@]} -lt 2 ]] &&
    die "parity needs at least two sources to compare; got ${#sources[@]}"

names=()
revs=()
for source in "${sources[@]}"; do
    rev="$(read_manifest "$source" | extract_rev)"
    [[ -n "$rev" ]] ||
        die "'$source' does not pin $CRATE_NAME to a git rev (is it still a consumer?)"
    names+=("$source")
    revs+=("$rev")
done

echo "Pinned $CRATE_NAME revisions:"
for i in "${!names[@]}"; do
    printf '  %-40s %s\n' "${names[$i]}" "${revs[$i]}"
done

for rev in "${revs[@]}"; do
    if [[ "$rev" != "${revs[0]}" ]]; then
        cat >&2 <<EOF

FAIL: consumers pin different $CRATE_NAME revisions.

The two capture clients are running different harness knowledge — different
attribution, different envelope composition — while both repos' tests pass.
Re-point every consumer at one revision (normally the newest that has landed
on this crate's main) and update each lockfile.
EOF
        exit 1
    fi
done

echo
echo "OK: all ${#revs[@]} consumers pin ${revs[0]}"
