#!/usr/bin/env bash
# Builds catmail-gui in release and packs it as an AppImage, with the .zsync file
# AppImageUpdate reads:
#
#   target/appimage/catmail-<version>-<arch>.AppImage
#   target/appimage/catmail-<version>-<arch>.AppImage.zsync
#
# Run it on the oldest glibc the AppImage is to run on (CI uses Ubuntu 22.04), with
# rustup's cargo: a build from the Nix dev shell links against the Nix glibc and loader,
# which the checks below refuse. Needs curl, patchelf, readelf and objdump.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
here=$repo/packaging/linux
# Absolute: cargo takes a relative one from the current directory, and packing below
# runs in another.
target_dir=$(realpath -m "${CARGO_TARGET_DIR:-$repo/target}")
export CARGO_TARGET_DIR=$target_dir
out=$target_dir/appimage
appdir=$out/AppDir
downloads=$out/downloads
arch=$(uname -m)

# The newest glibc symbol version the binary may need: Ubuntu 22.04's.
max_glibc=2.35
# Libraries every desktop has, taken from the system. Bundling them breaks more than it
# fixes: fontconfig reads the system's configuration, libxcb and libxkbcommon must match
# the display server's, and GL and Vulkan (loaded at run time) come with the drivers.
system_libs=(
  libc.so.6 libm.so.6 libpthread.so.0 libdl.so.2 librt.so.1 libgcc_s.so.1
  ld-linux-x86-64.so.2 ld-linux-aarch64.so.1
  libfontconfig.so.1 libxkbcommon.so.0
  libxcb.so.1 libXau.so.6 libXdmcp.so.6
)
# Only X11 needs these, so a Wayland-only system may not have them.
bundled_libs=(libxkbcommon-x11.so.0 libxcb-xkb.so.1)

case $arch in
  x86_64)
    interpreter=/lib64/ld-linux-x86-64.so.2
    appimagetool_sha256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
    runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
    ;;
  aarch64)
    interpreter=/lib/ld-linux-aarch64.so.1
    appimagetool_sha256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
    runtime_sha256=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
    ;;
  *)
    echo "no AppImage for $arch" >&2
    exit 1
    ;;
esac

die() {
  echo "error: $*" >&2
  exit 1
}

# fetch <url> <sha256> <file>: downloads once into $downloads, checked against the hash.
fetch() {
  local file=$downloads/$3
  if [[ -f $file ]] && echo "$2  $file" | sha256sum -c --quiet - 2>/dev/null; then
    return
  fi
  mkdir -p "$downloads"
  curl -fsSL --retry 3 -o "$file.part" "$1"
  echo "$2  $file.part" | sha256sum -c --quiet - || die "$3 doesn't match its hash"
  mv "$file.part" "$file"
}

needed() {
  readelf -d "$1" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'
}

# Pinned releases: appimagetool's `continuous` tag moves, and without --runtime-file it
# downloads the runtime's `continuous` at packing time.
fetch "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-$arch.AppImage" \
  "$appimagetool_sha256" "appimagetool-$arch.AppImage"
fetch "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-$arch" \
  "$runtime_sha256" "runtime-$arch"
# The CJK fonts the app looks for in a data directory (see docs/architecture/gui.md), the
# same files the dev shell fetches in flake.nix.
noto=https://github.com/notofonts/noto-cjk/raw/Sans2.004
fetch "$noto/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf" \
  faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9 NotoSansSC-Regular.otf
fetch "$noto/Sans/SubsetOTF/SC/NotoSansSC-Bold.otf" \
  c6cb5a93abaa9edc8ee7463b7ebb7f42d618d40e6ed2f7a5371c97b0b64767c0 NotoSansSC-Bold.otf
fetch "$noto/LICENSE" \
  6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2 NotoSansCJK-LICENSE

cargo build --release --locked -p catmail-gui --manifest-path "$repo/Cargo.toml"
bin=$target_dir/release/catmail-gui
pkgid=$(cargo pkgid -p catmail-gui --manifest-path "$repo/Cargo.toml")
version=${pkgid##*[#@]}

# Built against a glibc and loader any supported system has.
readelf -l "$bin" | grep -qF "[Requesting program interpreter: $interpreter]" ||
  die "catmail-gui doesn't use $interpreter: built outside a standard system (Nix?)"
glibc=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -n 1)
[[ $(printf '%s\n' "$glibc" "$max_glibc" | sort -V | tail -n 1) == "$max_glibc" ]] ||
  die "catmail-gui needs glibc $glibc, newer than $max_glibc"

rm -rf "$appdir"
install -Dm755 "$bin" "$appdir/usr/bin/catmail-gui"
for lib in "${bundled_libs[@]}"; do
  path=$(ldd "$bin" | awk -v lib="$lib" '$1 == lib { print $3 }')
  [[ -n $path ]] || die "$lib not found on this system"
  install -Dm644 "$path" "$appdir/usr/lib/$lib"
  # RUNPATH only covers an object's own dependencies: libxkbcommon-x11 finds
  # libxcb-xkb by its own.
  # shellcheck disable=SC2016 # $ORIGIN is the loader's, not the shell's
  patchelf --set-rpath '$ORIGIN' "$appdir/usr/lib/$lib"
done
# shellcheck disable=SC2016
patchelf --set-rpath '$ORIGIN/../lib' "$appdir/usr/bin/catmail-gui"

# A library that is neither bundled nor on every system fails at start on some systems.
for object in "$appdir/usr/bin/catmail-gui" "$appdir"/usr/lib/*; do
  for lib in $(needed "$object"); do
    [[ " ${system_libs[*]} ${bundled_libs[*]} " == *" $lib "* ]] ||
      die "$(basename "$object") needs $lib: bundle it or add it to the system libraries"
  done
done

install -Dm755 "$here/AppRun" "$appdir/AppRun"
install -Dm644 "$here/net.yangkx.catmail.desktop" "$appdir/net.yangkx.catmail.desktop"
install -Dm644 "$here/net.yangkx.catmail.desktop" \
  "$appdir/usr/share/applications/net.yangkx.catmail.desktop"
install -Dm644 "$here/net.yangkx.catmail.appdata.xml" \
  "$appdir/usr/share/metainfo/net.yangkx.catmail.appdata.xml"
# The fixed light icon: icon loaders don't follow the media query of catmail.svg.
icon=$repo/crates/catmail-gui/assets/app-icon/catmail-light.svg
install -Dm644 "$icon" "$appdir/net.yangkx.catmail.svg"
install -Dm644 "$icon" "$appdir/usr/share/icons/hicolor/scalable/apps/net.yangkx.catmail.svg"
ln -s net.yangkx.catmail.svg "$appdir/.DirIcon"
for face in Regular Bold; do
  install -Dm644 "$downloads/NotoSansSC-$face.otf" \
    "$appdir/usr/share/catmail/fonts/NotoSansSC-$face.otf"
done
install -Dm644 "$downloads/NotoSansCJK-LICENSE" \
  "$appdir/usr/share/licenses/catmail/NotoSansCJK-LICENSE"
install -Dm644 "$repo/crates/catmail-gui/assets/fonts/noto-sans/OFL.txt" \
  "$appdir/usr/share/licenses/catmail/NotoSans-OFL.txt"
install -Dm644 "$repo/LICENSE-MIT" "$appdir/usr/share/licenses/catmail/LICENSE-MIT"
install -Dm644 "$repo/LICENSE-APACHE" "$appdir/usr/share/licenses/catmail/LICENSE-APACHE"

appimage=catmail-$version-$arch.AppImage
rm -f "$out/$appimage" "$out/$appimage.zsync"
chmod +x "$downloads/appimagetool-$arch.AppImage"
# The updater finds the latest release's .zsync for this architecture by the pattern.
# appimagetool is an AppImage itself: extracting it needs no FUSE (CI has none).
(cd "$out" && APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$arch VERSION=$version \
  "$downloads/appimagetool-$arch.AppImage" \
  --runtime-file "$downloads/runtime-$arch" \
  --updateinformation "gh-releases-zsync|yangkx1024|cat-mail|latest|catmail-*-$arch.AppImage.zsync" \
  "$appdir" "$appimage")
[[ -f $out/$appimage.zsync ]] || die "appimagetool wrote no $appimage.zsync"
echo "$out/$appimage"
