#!/usr/bin/env bash
# Starts throwaway servers for the real-server tests: Dovecot with the fixtures in
# testenv/fixtures for IMAP (crates/catmail-core/tests/dovecot.rs), and Dovecot's
# submission service relaying to a mailpit sink for SMTP (tests/submission.rs).
# Everything lives in a temp directory that is removed on exit.
#
#   nix develop .#testenv -c testenv/dovecot/run.sh [port]
#
# Then, in another shell, run the exports it prints and
# `cargo test -p catmail-core --test dovecot --test submission`.
#
# Ports: IMAP `port` (10143), submission with STARTTLS port+444 (10587), submission with
# implicit TLS port+322 (10465), mailpit SMTP port+882 (11025) and HTTP API port+937
# (11080).
set -euo pipefail

port=${1:-10143}
submission=$((port + 444))
submissions=$((port + 322))
relay=$((port + 882))
api=$((port + 937))
here=$(cd "$(dirname "$0")" && pwd)
fixtures=$here/../fixtures
work=$(mktemp -d "${TMPDIR:-/tmp}/catmail-dovecot.XXXXXX")
# Dovecot's unix sockets live here; their paths must stay under ~100 bytes.
run=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/cmdv.XXXXXX")
trap 'kill "${pid:-}" "${mailpit:-}" 2>/dev/null || true; rm -rf "$work" "$run"' EXIT INT TERM

# For 127.0.0.1 itself, not a CA: rustls refuses a CA certificate as the server's.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 7 \
    -subj /CN=127.0.0.1 -addext subjectAltName=IP:127.0.0.1 \
    -addext basicConstraints=critical,CA:FALSE \
    -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
openssl x509 -in "$work/cert.pem" -outform der -out "$work/cert.der"

# Takes whatever Dovecot relays; tests read it back over the HTTP API. Recipients at
# refused.example are refused, so a test can see a real server turn one down.
mailpit --database "$work/mailpit.db" --smtp "127.0.0.1:$relay" --listen "127.0.0.1:$api" \
    --smtp-allowed-recipients '@example\.com$' --smtp-disable-rdns --disable-version-check \
    >&2 &
mailpit=$!
for _ in $(seq 1 100); do
    (exec 3<>"/dev/tcp/127.0.0.1/$api") 2>/dev/null && break
    sleep 0.1
done

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
cp "$fixtures/06-html-inline.eml"  "$maildir/new/1792200006.fixture06"
touch -d @1792300000 "$maildir/new/1792200005.fixture05" # its INTERNALDATE, 2026-10-18
cp "$fixtures/01-plain.eml"        "$maildir/.Archive.2026/cur/1792100000.archived:2,S"

# `catmail` has the fixtures (tests/dovecot.rs); `sender` sends through submission
# (tests/submission.rs), so the two never change the same mailbox; `push` gets mail
# delivered while a client idles on its INBOX (tests/dovecot.rs).
mkfolder "$work/mail/push/Maildir"
for user in catmail sender push; do
    echo "$user:{PLAIN}$user:$(id -u):$(id -g)::$work/mail/$user::"
done > "$work/users"
sed -e "s|@WORK@|$work|g" -e "s|@RUN@|$run|g" -e "s|@PORT@|$port|g" \
    -e "s|@SUBMISSION@|$submission|g" -e "s|@SUBMISSIONS@|$submissions|g" \
    -e "s|@RELAY@|$relay|g" \
    -e "s|@USER@|$(id -un)|g" -e "s|@GROUP@|$(id -gn)|g" \
    "$here/dovecot.conf.in" > "$work/dovecot.conf"

echo "export CATMAIL_TEST_IMAP=127.0.0.1:$port CATMAIL_TEST_MAILDIR=$maildir" \
    "CATMAIL_TEST_SUBMISSION=127.0.0.1:$submission" \
    "CATMAIL_TEST_SUBMISSIONS=127.0.0.1:$submissions" \
    "CATMAIL_TEST_CERT=$work/cert.der CATMAIL_TEST_MAILPIT=127.0.0.1:$api"
dovecot -F -c "$work/dovecot.conf" &
pid=$!
# Either one stopping ends the test environment.
wait -n "$pid" "$mailpit"
