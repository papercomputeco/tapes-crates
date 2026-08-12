#!/usr/bin/env bash
# sync-envelope-fixtures.sh — refresh the vendored copy of the shared
# envelope fixture corpus under crates/tapes-capture/vendor/tapes-envelope-fixtures/.
#
# The corpus is authored in the `tapes` repository at fixtures/envelope/
# and vendored here so this repo's tests need no cross-repo checkout.
# Vendoring means it can drift, so this script is both the refresher and
# the drift detector.
#
# This is a manual procedure, not automation — same shape as (and kept in
# step with) platform/paper's scripts/sync-envelope-fixtures.sh. It takes a
# local checkout path; no network is involved.
#
# Usage:
#   ./scripts/sync-envelope-fixtures.sh <path-to-tapes-checkout>
#   ./scripts/sync-envelope-fixtures.sh --check <path-to-tapes-checkout>
#
# --check writes nothing and exits non-zero if the vendored copy differs
# from upstream, printing the diff. Use it to decide whether a refresh is
# needed; the plain form performs it.
#
# The corpus ships a DIGEST sealing the case set. Both modes recompute it over
# the cases actually on disk and compare, so a hand-edit to one vendored copy
# is caught here rather than surviving as two implementations testing against
# different bytes while both stay green. A refresh whose staged corpus does not
# match the DIGEST it shipped with is refused outright: the live copy is left
# untouched rather than replaced with bytes nothing vouches for.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VENDOR_DIR="$REPO_ROOT/crates/tapes-capture/vendor/tapes-envelope-fixtures"

usage() {
    # Reprint this file's leading comment block as the help text: everything
    # from line 2 until the first non-comment line. Derived rather than
    # duplicated so help can't drift from the docs, and boundary-free so
    # editing the comment doesn't silently spill code into --help.
    awk 'NR > 1 { if (!/^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
    exit "${1:-0}"
}

# Recompute the corpus seal over a cases/ directory, printing `sha256:<hex>`.
#
# Upstream's algorithm, restated rather than imported: for each `cases/*.json`,
# sorted by base name, feed "<basename>  <sha256-hex-of-file-bytes>\n" into a
# SHA-256 and hex the result. It is deliberately trivial so every consumer can
# reimplement it from that one sentence without a canonical-JSON library — this
# script and the Rust seal test are two such reimplementations, and they agree
# with upstream's published value or the corpus does not install.
#
# It hashes RAW BYTES, not parsed JSON, because this script copies bytes: a
# reformat is drift too. It covers NAMES as well as contents, so an addition, a
# deletion, and a rename are each caught rather than only an edit to a file that
# already existed.
#
# LC_ALL=C pins byte-order sorting, matching the reference implementation's
# sort. Without it the caller's locale decides the order the hashes are fed in,
# and the same corpus digests differently on two machines.
corpus_digest() {
    local cases_dir="$1"
    local base
    while IFS= read -r base; do
        printf '%s  %s\n' "$base" "$(shasum -a 256 "$cases_dir/$base" | awk '{print $1}')"
    done < <(find "$cases_dir" -maxdepth 1 -type f -name '*.json' -exec basename {} \; |
        LC_ALL=C sort) | shasum -a 256 | awk '{print "sha256:" $1}'
}

check_only=false
case "${1:-}" in
    -h | --help | help | "")
        usage 0
        ;;
    --check)
        check_only=true
        shift
        ;;
esac

checkout_path="${1:-}"
if [[ -z "$checkout_path" ]]; then
    echo "error: missing path to a tapes checkout" >&2
    usage 2
fi

src_dir="$checkout_path/fixtures/envelope"
if [[ ! -d "$src_dir/cases" ]]; then
    echo "error: no envelope fixtures at $src_dir/cases" >&2
    echo "       (expected a tapes checkout; is the path right?)" >&2
    exit 2
fi

# The seal is not optional. A checkout predating it can still be vendored by
# hand, but not through this script: silently accepting a corpus with no DIGEST
# would install bytes the seal test then fails on, and the failure would name
# the vendored copy rather than the too-old checkout that produced it.
if [[ ! -f "$src_dir/DIGEST" ]]; then
    echo "error: no DIGEST at $src_dir/DIGEST" >&2
    echo "       the corpus has been sealed upstream since 7e3cdef; this checkout predates it" >&2
    exit 2
fi

# Pin the commit that last TOUCHED the fixtures, not the checkout's HEAD.
# HEAD moves for unrelated upstream work, which would churn the recorded SHA
# in SOURCE.md on every refresh even when not a byte of the corpus changed.
upstream_sha="$(git -C "$checkout_path" log -1 --format=%H -- fixtures/envelope 2>/dev/null || true)"
if [[ -z "$upstream_sha" ]]; then
    upstream_sha="$(git -C "$checkout_path" rev-parse HEAD 2>/dev/null || echo "unknown")"
fi

if $check_only; then
    status=0
    # Compare the case corpus as a directory so an upstream ADDITION or
    # DELETION is caught, not just an edit to a file we already have.
    if ! diff -ru "$VENDOR_DIR/cases" "$src_dir/cases"; then
        status=1
    fi
    if ! diff -u "$VENDOR_DIR/README.upstream.md" "$src_dir/README.md"; then
        status=1
    fi
    if ! diff -u "$VENDOR_DIR/DIGEST" "$src_dir/DIGEST"; then
        status=1
    fi
    # Independent of the diffs above: recompute the seal over the vendored
    # cases and hold it against the vendored DIGEST. The diffs answer "does
    # this match upstream"; this answers "does this copy match its own seal",
    # which is the question the consumer's CI asks when no checkout is around.
    if [[ -f "$VENDOR_DIR/DIGEST" ]]; then
        vendored_recomputed="$(corpus_digest "$VENDOR_DIR/cases")"
        vendored_sealed="$(tr -d '[:space:]' < "$VENDOR_DIR/DIGEST")"
        if [[ "$vendored_recomputed" != "$vendored_sealed" ]]; then
            echo >&2
            echo "error: the vendored corpus does not match its own DIGEST" >&2
            echo "       sealed:     $vendored_sealed" >&2
            echo "       recomputed: $vendored_recomputed" >&2
            echo "       a case here has been hand-edited; re-sync rather than editing in place" >&2
            status=1
        fi
    fi
    if [[ $status -eq 0 ]]; then
        echo "vendored envelope fixtures match $checkout_path @ $upstream_sha"
        echo "seal verified: $vendored_recomputed"
    else
        echo >&2
        echo "error: vendored envelope fixtures differ from $checkout_path @ $upstream_sha" >&2
        echo "       run: $0 $checkout_path" >&2
    fi
    exit $status
fi

# Refresh via stage-then-swap. The vendored copy has to be *replaced* rather
# than merged — an upstream deletion must propagate instead of leaving a stale
# case behind that nothing upstream describes any more — but deleting first
# means any failure after that point (an empty source, an unreadable case, a
# missing README) leaves no corpus at all, and the fixture tests red, until
# someone restores it by hand.
#
# So every fallible step happens in a staging directory, and the live one is
# only touched once the replacement is known-good. On failure the old copy is
# put back.
src_cases=()
for f in "$src_dir"/cases/*.json; do
    [[ -f "$f" ]] || continue
    src_cases+=("$f")
done
if [[ ${#src_cases[@]} -eq 0 ]]; then
    echo "error: no case files at $src_dir/cases" >&2
    echo "       refusing to refresh; the vendored copy is left untouched" >&2
    exit 2
fi

# Stage beside the target, NOT in TMPDIR. mv is only atomic within a
# filesystem; across one it degrades to copy-then-delete, which can fail
# partway and leave a half-built directory where the vendored corpus should
# be. The rollback below would then move the backup *into* that directory
# rather than restoring it, turning a failed refresh into a corrupted tree.
# Same parent, same filesystem, real rename.
#
# A SIGKILL can leave one of these behind; anything less is covered by the
# trap.
mkdir -p "$(dirname "$VENDOR_DIR")"
staging="$(mktemp -d "$(dirname "$VENDOR_DIR")/.sync-envelope-fixtures.XXXXXX")"
previous=""

# The swap below is two renames, and an interrupt landing between them would
# otherwise leave no vendored corpus at all: the live directory moved aside,
# the replacement not yet installed. So cleanup restores rather than only
# tidying, and runs on signals as well as on exit.
#
# Idempotent by construction — it only acts when the live directory is absent
# and a backup exists — so running it from both an INT handler and the EXIT
# trap is harmless. SIGKILL is still unrecoverable; nothing in shell can help
# there, and the backup is left in place for a human.
cleanup() {
    if [[ -n "$previous" && -d "$previous" && ! -e "$VENDOR_DIR" ]]; then
        mv "$previous" "$VENDOR_DIR" || true
    fi
    [[ -n "$staging" && -d "$staging" ]] && rm -rf "$staging"
    return 0
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

mkdir -p "$staging/cases"
cp "${src_cases[@]}" "$staging/cases/"
cp "$src_dir/README.md" "$staging/README.upstream.md"
cp "$src_dir/DIGEST" "$staging/DIGEST"

# Verify the seal against what was actually staged, before anything is swapped
# in. This is the one point where a copy error, a partially-written case, or an
# upstream DIGEST that never matched its own cases is still cheap to refuse —
# afterwards the live corpus is already gone. Fail here and the vendored copy is
# untouched, which is why it runs before the renames rather than after them.
staged_digest="$(corpus_digest "$staging/cases")"
upstream_digest="$(tr -d '[:space:]' < "$staging/DIGEST")"
if [[ "$staged_digest" != "$upstream_digest" ]]; then
    echo "error: the staged corpus does not match the DIGEST it shipped with" >&2
    echo "       upstream:   $upstream_digest" >&2
    echo "       recomputed: $staged_digest" >&2
    echo "       refusing to install; the vendored copy is left untouched" >&2
    exit 1
fi
# SOURCE.md is authored here, not upstream: carry it across so the swap does
# not drop it.
if [[ -f "$VENDOR_DIR/SOURCE.md" ]]; then
    cp "$VENDOR_DIR/SOURCE.md" "$staging/SOURCE.md"
fi

# Swap. Renames only from here on, so the window where neither copy is in
# place is as small as it can be made in shell, and it is recoverable.
if [[ -d "$VENDOR_DIR" ]]; then
    previous="${VENDOR_DIR}.previous.$$"
    mv "$VENDOR_DIR" "$previous"
fi
if ! mv "$staging" "$VENDOR_DIR"; then
    echo "error: could not install the refreshed corpus" >&2
    # Only restore onto empty ground. If the failed rename somehow left
    # something behind, moving the backup would nest it inside rather than
    # replace it — say so and leave both in place for a human.
    if [[ -e "$VENDOR_DIR" ]]; then
        echo "       $VENDOR_DIR still exists; previous copy preserved at $previous" >&2
    elif [[ -d "$previous" ]]; then
        mv "$previous" "$VENDOR_DIR"
        echo "       previous copy restored" >&2
    fi
    exit 1
fi
rm -rf "$previous"

echo "Synced $(find "$VENDOR_DIR/cases" -name '*.json' | wc -l | tr -d ' ') cases from $src_dir"
echo "Upstream SHA: $upstream_sha"
echo "Seal:         $staged_digest (verified against the corpus as staged)"
echo
echo "Now:"
echo "  1. Record that SHA in $VENDOR_DIR/SOURCE.md."
echo "  2. Run: cargo test -p tapes-capture --all-features"
echo "  3. If the parser changed behaviour, land the fixture bump and the"
echo "     parser change in the same PR — the corpus is the contract."
echo "  4. Refresh the OTHER vendored copies from the SAME SHA:"
echo "       - platform/paper       crates/paper-daemon/vendor/tapes-envelope-fixtures/"
echo "       - tapes-extproc        internal/headers/testdata/envelope/"
