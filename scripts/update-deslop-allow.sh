#!/usr/bin/env sh
# Writes .deslop-please.allow: what deslop-please finds on each OS CI checks, merged.
# A run indexes one configuration, and code behind another OS's `cfg` isn't in it, so
# each OS's findings are found with `--target` (from any host) and the lists joined.
# Only to drop entries a fix removed: a new finding is fixed, not listed.
set -eu

cd "$(dirname "$0")/.."
tmp=$(mktemp -d "${TMPDIR:-/tmp}/deslop-allow.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

for target in aarch64-apple-darwin x86_64-unknown-linux-gnu x86_64-pc-windows-msvc; do
    deslop-please --target "$target" --update-allow "$tmp/$target.allow"
done

{
    printf '%s\n' \
        "# Existing architectural debt suppressed by \`deslop-please --allow\`, on every OS CI" \
        "# checks; written by scripts/update-deslop-allow.sh. Fixing an entry never fails CI;" \
        "# new issues not listed here fail."
    cat "$tmp"/*.allow | grep -v '^#' | LC_ALL=C sort -u
} > .deslop-please.allow
