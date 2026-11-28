# shellcheck shell=bash
# shellcheck disable=SC2034,SC2154 # sets variables for the scripts that source it, from their root
#
# Sourced by the Flatpak scripts next to it, after `die` and with `root` set to the
# checkout: what the packaging files say about the app, read from them rather than
# written into each script.
#
#   manifest  the app's manifest, the one with an id (APP_ID in catmail-core), not a module's
#   app_id    its id
#   branch    the branch it's exported to
#   repo_file / ref_file  the .flatpakrepo (adds the repository) and .flatpakref (installs
#                         the app from it), which update-flatpak-repo.sh serves with it
#   runtime_repo          where the GNOME runtime comes from (Flathub)

manifest=$(grep -l '^id: ' "$root"/packaging/linux/*.yml) || die "no manifest in packaging/linux"
app_id=$(sed -n 's/^id: *//p' "$manifest")
branch=$(sed -n 's/^branch: *//p' "$manifest")
[[ -n $branch ]] || die "$manifest names no branch"
repo_file=$root/packaging/linux/$app_id.flatpakrepo
ref_file=$root/packaging/linux/$app_id.flatpakref
runtime_repo=$(sed -n 's/^RuntimeRepo=//p' "$ref_file")
[[ -n $runtime_repo ]] || die "$ref_file names no RuntimeRepo"
