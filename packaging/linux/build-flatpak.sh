#!/usr/bin/env bash
# Builds catmail as a Flatpak (GNOME runtime) into target/flatpak/:
# catmail-<version>-<arch>.flatpak, a single-file bundle, and the repo it was exported to.
#
# Needs flatpak, flatpak-builder (or the org.flatpak.Builder app), cargo, and the
# runtime, SDK and Rust extension the manifest names (installed from Flathub with
# --install-deps-from when missing).
#
#   packaging/linux/build-flatpak.sh            build, install for the user, bundle
#   packaging/linux/build-flatpak.sh --no-install
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
manifest=packaging/linux/net.yangkx.catmail.yml
out=target/flatpak
install=--install
[[ ${1:-} == --no-install ]] && install=

die() { echo "error: $*" >&2; exit 1; }

if command -v flatpak-builder > /dev/null; then
  builder=(flatpak-builder)
elif flatpak info org.flatpak.Builder > /dev/null 2>&1; then
  builder=(flatpak run --filesystem="$root" org.flatpak.Builder)
else
  die "needs flatpak-builder (flatpak install flathub org.flatpak.Builder)"
fi

pkgid=$(cargo pkgid -p catmail-gui)
version=${pkgid##*[#@]}
arch=$(flatpak --default-arch)

# The build runs without network: vendor every crate, crates.io's and the git ones,
# and point cargo at them from the build directory.
mkdir -p "$out"
rm -rf "$out/vendor"
cargo vendor --locked --versioned-dirs "$out/vendor" |
  sed 's|^directory = .*|directory = "/run/build/catmail/vendor"|' > "$out/cargo-config.toml"
grep -q '^directory = "/run/build/catmail/vendor"$' "$out/cargo-config.toml" ||
  die "unexpected cargo vendor output"

"${builder[@]}" --force-clean --install-deps-from=flathub \
  --state-dir="$out/state" --repo="$out/repo" --user $install \
  "$out/build" "$manifest"

bundle=$out/catmail-$version-$arch.flatpak
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$out/repo" "$bundle" net.yangkx.catmail
echo "$bundle"
