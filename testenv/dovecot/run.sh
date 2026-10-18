#!/usr/bin/env bash
# Starts a throwaway Dovecot with the fixtures in testenv/fixtures, for the real-server
# tests (crates/catmail-core/tests/dovecot.rs). Everything lives in a temp directory
# that is removed on exit.
#
#   nix develop .#imap -c testenv/dovecot/run.sh [port]
#
# Then, in another shell, run the exports it prints and `cargo test --test dovecot`.
set -euo pipefail

port=${1:-10143}
here=$(cd "$(dirname "$0")" && pwd)
fixtures=$here/../fixtures
work=$(mktemp -d "${TMPDIR:-/tmp}/catmail-dovecot.XXXXXX")
# Dovecot's unix sockets live here; their paths must stay under ~100 bytes.
run=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/cmdv.XXXXXX")
trap 'kill "${pid:-}" 2>/dev/null || true; rm -rf "$work" "$run"' EXIT INT TERM

maildir=$work/mail/catmail/Maildir
mkfolder() { mkdir -p "$1"/{cur,new,tmp}; }
mkfolder "$maildir"
mkfolder "$maildir/.Archive.2026"        # "Archive" exists only as a hierarchy level
mkfolder "$maildir/.&V4NXPpCuTvY-"       # 垃圾邮件, in modified UTF-7

# Maildir file names carry the flags: S = \Seen, F = \Flagged.
cp "$fixtures/01-plain.eml"        "$maildir/cur/1792200001.fixture01:2,S"
cp "$fixtures/02-utf8-subject.eml" "$maildir/cur/1792200002.fixture02:2,F"
cp "$fixtures/03-attachment.eml"   "$maildir/new/1792200003.fixture03"
cp "$fixtures/04-gb2312.eml"       "$maildir/new/1792200004.fixture04"
cp "$fixtures/05-no-date.eml"      "$maildir/new/1792200005.fixture05"
touch -d @1792300000 "$maildir/new/1792200005.fixture05" # its INTERNALDATE, 2026-10-18
cp "$fixtures/01-plain.eml"        "$maildir/.Archive.2026/cur/1792100000.archived:2,S"

echo "catmail:{PLAIN}catmail:$(id -u):$(id -g)::$work/mail/catmail::" > "$work/users"
sed -e "s|@WORK@|$work|g" -e "s|@RUN@|$run|g" -e "s|@PORT@|$port|g" \
    -e "s|@USER@|$(id -un)|g" -e "s|@GROUP@|$(id -gn)|g" \
    "$here/dovecot.conf.in" > "$work/dovecot.conf"

echo "export CATMAIL_TEST_IMAP=127.0.0.1:$port CATMAIL_TEST_MAILDIR=$maildir"
dovecot -F -c "$work/dovecot.conf" &
pid=$!
wait "$pid"
