#!/usr/bin/env bash
# catmail's Flatpak repository (docs/architecture/packaging.md, "仓库与更新"):
#
#   packaging/linux/update-flatpak-repo.sh [--test-key] <repo> <bundle>...
#   packaging/linux/update-flatpak-repo.sh --check <version>
#
# The first adds the bundles build-flatpak.sh made to <repo>, a local copy of the
# repository (a missing or empty directory starts one): each bundle's commit goes on top
# of its ref, signed, with static deltas from the three versions before it; older commits
# are pruned and the summary signed. The bundles to publish, in target/flatpak/signed/,
# hold those commits and name the repository, so an install from one updates from it.
# Then it checks <repo> as an installed app would.
#
# --check checks the published repository the same way, through the .flatpakrepo it
# serves: every arch's ref must offer <version>.
#
# Needs flatpak, ostree and gpg, with the repository key's private half in GNUPGHOME, or
# --test-key to sign with a key made for the run.
#   CATMAIL_FLATPAK_KEY  the public key (default packaging/linux/catmail-flatpak.gpg)
#   CATMAIL_FLATPAK_URL  the repository's address the bundles name (default the
#                        .flatpakrepo's Url); a file:// one to try updates locally
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
die() { echo "error: $*" >&2; exit 1; }
# shellcheck source=packaging/linux/manifest.sh
. "$root/packaging/linux/manifest.sh"

for tool in flatpak ostree gpg; do
  command -v "$tool" > /dev/null || die "needs $tool"
done
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Adds the repository from a .flatpakrepo (whose key checks the summary's signature) to
# an installation of its own, and checks that each arch's ref offers <version> (any, when
# empty). remote-ls takes the version from the arch's AppStream data: that comes first.
check() {
  local from=$1 version=$2 installation ref arch offered
  installation=$(mktemp -d -p "$work")
  FLATPAK_USER_DIR=$installation flatpak remote-add --user --from catmail "$from"
  local refs
  mapfile -t refs < <(FLATPAK_USER_DIR=$installation flatpak remote-ls --user --arch='*' \
    --columns=ref catmail | grep -x "app/$app_id/[^/]*/$branch")
  [[ ${#refs[@]} -gt 0 ]] || die "$from offers no $app_id $branch"
  for ref in "${refs[@]}"; do
    arch=$(cut -d/ -f3 <<< "$ref")
    FLATPAK_USER_DIR=$installation flatpak update --user --noninteractive --appstream \
      --arch="$arch" catmail > /dev/null
    offered=$(FLATPAK_USER_DIR=$installation flatpak remote-ls --user --arch="$arch" \
      --columns=ref,version catmail | awk -v ref="$ref" '$1 == ref { print $2 }')
    echo "$ref ${offered:-(no version)}"
    [[ -z $version || $offered == "$version" ]] || die "$ref offers ${offered:-no version}, not $version"
  done
}

if [[ ${1:-} == --check ]]; then
  [[ $# -eq 2 && -n $2 ]] || die "usage: $0 --check <version>"
  check "$(sed -n 's/^Url=//p' "$repo_file")$app_id.flatpakrepo" "$2"
  exit
fi

key=${CATMAIL_FLATPAK_KEY:-$root/packaging/linux/catmail-flatpak.gpg}
if [[ ${1:-} == --test-key ]]; then
  shift
  export GNUPGHOME=$work/gnupg
  mkdir -m 700 "$GNUPGHOME"
  trap 'gpgconf --kill gpg-agent; rm -rf "$work"' EXIT
  gpg --batch --quiet --pinentry-mode loopback --passphrase '' \
    --quick-gen-key 'catmail Flatpak repository (test)' ed25519 sign never
  key=$work/test-key.gpg
  gpg --export > "$key"
fi
[[ $# -ge 2 ]] || die "usage: $0 [--test-key] <repo> <bundle>..."
repo=$1
shift
url=${CATMAIL_FLATPAK_URL:-$(sed -n 's/^Url=//p' "$repo_file")}
[[ -n $url ]] || die "no repository URL"

fingerprint=$(gpg --show-keys --with-colons "$key" | awk -F: '$1 == "fpr" { print $10; exit }')
[[ -n $fingerprint ]] || die "$key holds no key"
gpg --list-secret-keys "$fingerprint" > /dev/null 2>&1 ||
  die "no private key for $fingerprint in ${GNUPGHOME:-~/.gnupg}"

if [[ ! -e $repo/config ]]; then
  mkdir -p "$repo"
  [[ -z $(ls -A "$repo") ]] || die "$repo is neither a repository nor empty"
  ostree init --repo="$repo" --mode=archive-z2
fi
out=$root/target/flatpak/signed
rm -rf "$out"
mkdir -p "$out"

# Each bundle goes through a repository of its own, holding only its ref, to the
# repository's ref: build-commit-from makes the new commit a child of the one there,
# which the deltas and pruning follow. The same files as the ref's last commit (the same
# release again) add no commit. The bundle to publish holds that signed commit: one that
# carries the key makes flatpak require it.
declare -A refs=()
for bundle in "$@"; do
  staging=$work/staging-${#refs[@]}
  ostree init --repo="$staging" --mode=archive-z2
  flatpak build-import-bundle --no-update-summary "$staging" "$bundle"
  ref=$(ostree refs --repo="$staging" | grep -x "app/$app_id/[^/]*/$branch" || true)
  [[ -n $ref ]] || die "$bundle holds no $app_id $branch"
  [[ -z ${refs[$ref]:-} ]] || die "two bundles of $ref"
  refs[$ref]=1
  flatpak build-commit-from --no-update-summary --src-repo="$staging" \
    --gpg-sign="$fingerprint" "$repo" "$ref"
  flatpak build-bundle --arch="$(cut -d/ -f3 <<< "$ref")" --repo-url="$url" --gpg-keys="$key" \
    --runtime-repo="$runtime_repo" "$repo" "$out/$(basename "$bundle")" "$app_id" "$branch"
done

update=(flatpak build-update-repo --gpg-sign="$fingerprint"
  --title="$(sed -n 's/^Title=//p' "$repo_file")" --default-branch="$branch")
# Deltas to each ref's new commit from its parent (and from nothing), deleting any other
# delta, and three versions kept before the newest, for the two deltas added next.
"${update[@]}" --generate-static-deltas --prune --prune-depth=3 "$repo"
# Deltas from the two versions before the parent; only now, or the step above deletes
# them. Each bsdiffs the binaries on one core, so they run side by side.
pids=()
for ref in "${!refs[@]}"; do
  mapfile -t commits < <(ostree log --repo="$repo" "$ref" | sed -n 's/^commit //p')
  for older in "${commits[@]:2:2}"; do
    ostree static-delta generate --repo="$repo" --from="$older" --to="${commits[0]}" &
    pids+=($!)
  done
done
for pid in "${pids[@]}"; do
  wait "$pid"
done
# The delta index and the summary again, signed, with those deltas.
"${update[@]}" --no-update-appstream "$repo"

# Served next to the repository, for `flatpak install <url>.flatpakref`.
cp "$repo_file" "$ref_file" "$repo/"

local_repo_file=$work/local.flatpakrepo
sed -e "s|^Url=.*|Url=file://$(cd "$repo" && pwd)/|" \
  -e "s|^GPGKey=.*|GPGKey=$(base64 -w 0 "$key")|" "$repo_file" > "$local_repo_file"
metainfo=$root/packaging/linux/$app_id.appdata.xml
check "$local_repo_file" "$(sed -n 's/^ *<release version="\([^"]*\)".*/\1/p' "$metainfo" | head -n 1)"
ls -l "$out"
