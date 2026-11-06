#!/usr/bin/env bash
# Builds catmail-gui and the catmail CLI in release for Apple silicon, bundles the GUI as
# catmail.app in a disk image and packs the CLI as a tarball:
#
#   target/macos/catmail-<version>-aarch64.dmg
#   target/macos/catmail-<version>-aarch64-macos.tar.gz
#
# Signs with the Developer ID Application identity CATMAIL_SIGN_IDENTITY (a name or
# SHA-1 in the keychain) of the team CATMAIL_TEAM_ID, and has Apple notarize the result
# when CATMAIL_NOTARY_KEY (an App Store Connect API key file), CATMAIL_NOTARY_KEY_ID and
# CATMAIL_NOTARY_ISSUER are set too. Without an identity it signs ad hoc: a package that
# runs on this Mac only, enough to check the packaging. Needs Xcode (codesign,
# notarytool).
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
here=$repo/packaging/macos
target_dir=${CARGO_TARGET_DIR:-$repo/target}
[[ $target_dir == /* ]] || target_dir=$PWD/$target_dir
export CARGO_TARGET_DIR=$target_dir
out=$target_dir/macos
tools=$out/tools
arch=aarch64

# The oldest macOS the app says it runs on (Info.plist), and the one the binaries are
# linked for. Not tried on anything older than the build machine.
min_macos=12.0
export MACOSX_DEPLOYMENT_TARGET=$min_macos
# Renders the icon. From crates.io, pinned.
resvg_version=0.49.0

identity=${CATMAIL_SIGN_IDENTITY:-}
team=${CATMAIL_TEAM_ID:-}
notary_key=${CATMAIL_NOTARY_KEY:-}

die() {
  echo "error: $*" >&2
  exit 1
}

[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || die "build on an Apple silicon Mac"
[[ -z $identity || -n $team ]] || die "CATMAIL_SIGN_IDENTITY needs CATMAIL_TEAM_ID"
if [[ -n $notary_key ]]; then
  [[ -n $identity ]] || die "only a Developer ID signature can be notarized"
  [[ -n ${CATMAIL_NOTARY_KEY_ID:-} && -n ${CATMAIL_NOTARY_ISSUER:-} ]] ||
    die "CATMAIL_NOTARY_KEY needs CATMAIL_NOTARY_KEY_ID and CATMAIL_NOTARY_ISSUER"
fi

# check_binary <file>: built for this architecture and macOS, and linked only against
# what every Mac has. A library from Homebrew or Nix would be missing on the user's Mac.
check_binary() {
  local name lib
  name=$(basename "$1")
  [[ $(lipo -archs "$1") == arm64 ]] || die "$name isn't built for arm64 alone"
  vtool -show-build "$1" | grep -qE "^ +minos $min_macos$" ||
    die "$name isn't built for macOS $min_macos"
  while read -r lib; do
    [[ $lib == /System/Library/* || $lib == /usr/lib/* ]] ||
      die "$name links against $lib, which isn't part of macOS"
  done < <(otool -L "$1" | tail -n +2 | awk '{ print $1 }')
}

# sign <codesign options> <path>: the hardened runtime notarization asks for. A secure
# timestamp needs Apple's server, and an ad hoc signature can't have one.
sign() {
  if [[ -n $identity ]]; then
    codesign --force --options runtime --timestamp --sign "$identity" "$@"
  else
    codesign --force --options runtime --sign - "$@"
  fi
}

# check_signature <path>
check_signature() {
  codesign --verify --strict --deep "$1"
  if [[ -n $identity ]]; then
    codesign --display --verbose=2 "$1" 2>&1 | grep -qx "TeamIdentifier=$team" ||
      die "$(basename "$1") isn't signed by the team $team"
  fi
}

# notarize <file>: submits a zip or disk image and waits for Apple's verdict, printing
# the notary's log when it is anything but Accepted.
notarize() {
  local credentials=(--key "$notary_key" --key-id "$CATMAIL_NOTARY_KEY_ID"
    --issuer "$CATMAIL_NOTARY_ISSUER")
  local result=$out/notarization.plist status id
  # The verdict decides, not the exit status: read it even when the submission failed.
  xcrun notarytool submit "$1" "${credentials[@]}" --wait --timeout 1h \
    --output-format plist > "$result" || true
  status=$(plutil -extract status raw "$result" 2>/dev/null) ||
    die "notarytool gave no verdict on $(basename "$1")"
  if [[ $status != Accepted ]]; then
    id=$(plutil -extract id raw "$result")
    xcrun notarytool log "$id" "${credentials[@]}" >&2 || true
    die "notarizing $(basename "$1") ended $status"
  fi
  echo "notarized $(basename "$1")"
}

resvg=$tools/bin/resvg
if [[ ! -x $resvg || $("$resvg" --version) != "$resvg_version" ]]; then
  CARGO_TARGET_DIR=$tools/build cargo install --locked --quiet --root "$tools" \
    --version "$resvg_version" resvg
fi

cargo build --release --locked -p catmail-gui --manifest-path "$repo/Cargo.toml"
cargo build --release --locked -p catmail-cli --manifest-path "$repo/Cargo.toml"
gui=$target_dir/release/catmail-gui
cli=$target_dir/release/catmail
pkgid=$(cargo pkgid -p catmail-gui --manifest-path "$repo/Cargo.toml")
version=${pkgid##*[#@]}
check_binary "$gui"
check_binary "$cli"

rm -rf "$out/stage"
stage=$out/stage
app=$stage/catmail.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/licenses"
sed -e "s/@VERSION@/$version/g" -e "s/@MIN_MACOS@/$min_macos/g" "$here/Info.plist" \
  > "$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"
cp "$gui" "$app/Contents/MacOS/catmail-gui"
cp "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$app/Contents/Resources/licenses/"

# The fixed light icon, on Apple's icon grid: the tile (16 to 496 of 512 in the source)
# takes 824 of 1024 pixels, centred, leaving the margin the system's icons have.
iconset=$out/catmail.iconset
rm -rf "$iconset"
mkdir -p "$iconset"
source_viewbox='width="512" height="512" viewBox="0 0 512 512"'
grid_viewbox='width="1024" height="1024" viewBox="-42.2524 -42.2524 596.5049 596.5049"'
icon_svg=$out/catmail-icon.svg
sed "1s/$source_viewbox/$grid_viewbox/" "$repo/crates/catmail-gui/assets/app-icon/catmail-light.svg" \
  > "$icon_svg"
grep -qF "$grid_viewbox" "$icon_svg" || die "catmail-light.svg no longer starts with $source_viewbox"
for size in 16 32 128 256 512; do
  "$resvg" -w "$size" -h "$size" "$icon_svg" "$iconset/icon_${size}x${size}.png"
  "$resvg" -w $((size * 2)) -h $((size * 2)) "$icon_svg" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output "$app/Contents/Resources/catmail.icns" "$iconset"

sign "$app"
check_signature "$app"
cli_dir=$out/cli/catmail-$version-$arch-macos
rm -rf "$out/cli"
mkdir -p "$cli_dir"
cp "$cli" "$cli_dir/catmail"
sign --identifier net.yangkx.catmail.cli "$cli_dir/catmail"
check_signature "$cli_dir/catmail"
cp "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$cli_dir/"

# The app and the CLI go to the notary together. The ticket is stapled to the app, so it
# opens offline too once copied out of the disk image; a bare executable can't hold one,
# and Gatekeeper looks the CLI's up online.
if [[ -n $notary_key ]]; then
  submission=$out/notarize
  rm -rf "$submission" "$submission.zip"
  mkdir -p "$submission"
  ditto "$app" "$submission/catmail.app"
  cp "$cli_dir/catmail" "$submission/catmail"
  ditto -c -k --keepParent "$submission" "$submission.zip"
  notarize "$submission.zip"
  rm -rf "$submission" "$submission.zip"
  xcrun stapler staple -q "$app"
fi

dmg=$out/catmail-$version-$arch.dmg
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
# hdiutil now and then fails with "Resource busy" on CI machines; a retry gets through.
for attempt in 1 2 3; do
  if hdiutil create -quiet -volname catmail -srcfolder "$stage" -fs HFS+ -format ULFO \
    -ov "$dmg"; then
    break
  fi
  [[ $attempt -lt 3 ]] || die "hdiutil couldn't create the disk image"
  sleep 5
done
if [[ -n $identity ]]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  check_signature "$dmg"
fi
if [[ -n $notary_key ]]; then
  notarize "$dmg"
  xcrun stapler staple -q "$dmg"
  xcrun stapler validate -q "$dmg"
  # What Gatekeeper decides for a downloaded disk image and the app in it.
  spctl --assess --verbose --type open --context context:primary-signature "$dmg"
  spctl --assess --verbose --type execute "$app"
fi

tarball=$out/catmail-$version-$arch-macos.tar.gz
# Without AppleDouble `._` files for extended attributes.
COPYFILE_DISABLE=1 tar -C "$out/cli" -czf "$tarball" "$(basename "$cli_dir")"
echo "$dmg"
echo "$tarball"
