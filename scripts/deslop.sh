#!/usr/bin/env sh
# deslop-please on every OS the app is built for, from any host: a run indexes one
# configuration, and code behind another OS's `cfg` isn't in it, so each OS is indexed
# with `--target`. All share .deslop-please.allow, what any of them finds.
#
#   scripts/deslop.sh           check each OS against the list (what CI runs)
#   scripts/deslop.sh --update  write the list again, each OS's findings merged; only to
#                               drop entries a fix removed: a new finding is fixed, not
#                               listed
set -eu

cd "$(dirname "$0")/.."
targets="aarch64-apple-darwin x86_64-unknown-linux-gnu x86_64-pc-windows-msvc"

if [ "${1:-}" != "--update" ]; then
    failed=
    for target in $targets; do
        echo "== $target"
        deslop-please --target "$target" --allow .deslop-please.allow || failed=1
    done
    [ -z "$failed" ]
    exit
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/deslop-allow.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
for target in $targets; do
    deslop-please --target "$target" --update-allow "$tmp/$target.allow"
done
{
    printf '%s\n' \
        "# Existing architectural debt suppressed by \`deslop-please --allow\`, on every OS;" \
        "# written by \`scripts/deslop.sh --update\`. Fixing an entry never fails CI; new" \
        "# issues not listed here fail."
    cat "$tmp"/*.allow | grep -v '^#' | LC_ALL=C sort -u
} > .deslop-please.allow
