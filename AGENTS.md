# cat-mail — Agent Guide

cat-mail is a cross-platform email client in Rust (Linux first, then Windows and macOS).
A UI-independent engine exposes a single `MailClient` facade; the `catmail` CLI and the
GPUI/gpui-kit GUI are peer frontends on top of it.

- Scope, roadmap and status: [README.md](README.md)
- Architecture and the reasoning behind it: [docs/architecture.md](docs/architecture.md)
  holds the layering and the cross-cutting design; each subsystem's details live in
  [docs/architecture/](docs/architecture/). Read the relevant document before changing a
  subsystem, and update it when a design decision changes. Keep `architecture.md` high
  level: implementation details go in the subsystem documents.

## Build and test

The toolchain comes from the Nix flake. Run cargo through it:

```sh
nix develop -c cargo test --workspace
nix develop -c cargo clippy --workspace --all-targets -- -D warnings
nix develop -c cargo fmt --all
```

Flakes only see git-tracked files: `git add -N` new files before `nix develop` / `nix build`.

Before reporting work as done, `cargo fmt`, `clippy -D warnings` and the full test suite
must pass. Say so explicitly if any of them could not be run.

CI (`.github/workflows/ci.yml`) runs clippy on Linux, macOS and Windows; pull requests
run the tests on Linux only, and `main` runs them on all three. A platform-specific
failure can therefore first show up after merging: when a change touches paths, files
or anything behind `cfg`, run the macOS / Windows tests before merging with
`gh workflow run ci.yml --ref <branch>`. Only Linux is shipped for now, but code must build and
pass everywhere: put
platform-specific code behind `cfg`, and don't assume Unix paths or file semantics in
code or tests. `nix.yml` checks `flake.nix` formatting (`nix fmt`) and the dev shell.

## Crates and layering

`types ← store, proto ← core ← cli / gui`

| Crate | Role |
|---|---|
| `catmail-types` | Domain types. Pure data: no IO, no async |
| `catmail-store` | SQLite schema, migrations, repositories, content-addressed `BlobStore`. Synchronous |
| `catmail-proto` | IMAP (`imap-next`), SMTP, MIME adapters |
| `catmail-core` | Engine, services, runtime bridge, secrets, the `MailClient` facade |
| `catmail-cli` | The `catmail` binary |
| `catmail-gui` | The `catmail-gui` desktop binary, on GPUI Kit |
| `catmail-testkit` | Fake IMAP and SMTP servers with fault injection; dev-dependency only |

Frontends depend on `catmail-core` only; it re-exports the types they need.

`catmail-gui` depends on `gpui-kit` alone, never on `gpui` directly. Before changing it,
read the vendored GPUI Kit skills in `crates/catmail-gui/.claude/skills/` (`gpui-kit` holds
the Coding Guides, `gpui-kit-design-guides` the Design Guides; see their `README.md` for
the upstream commit).

Protocol behaviour is tested against `catmail-testkit`'s fake servers (every OS) and a real
Dovecot (`testenv/dovecot/run.sh`, Linux CI job; IMAP, and SMTP submission relaying to a
mailpit sink). New sync or protocol behaviour gets a
fake-server test, with a fault injected if the change is about recovering from one.

## Rules that are easy to break

- **Runtime bridge.** Public `MailClient` methods must stay executor-agnostic: run work on
  the engine runtime (`Runtime::run` / `run_blocking`) and await the handle. Never call
  `tokio::spawn` or block on tokio from frontend code; GPUI is not tokio.
- **Database access** goes through `Store::read` / `Store::write`. Everything that must be
  atomic (a local change plus its `pending_ops` row, synced messages plus the folder
  cursor) happens inside one `write` closure. Never call back into the `Store` from inside
  that closure (single connection, it deadlocks).
- **Events** are emitted after the commit, from inside the engine task that made the
  change, never from inside a `write` closure and never after an `.await` in the caller's
  future.
- **Messages are per mailbox.** A `messages` row is one IMAP (folder, UID) pair with its own
  flags. Don't merge copies across folders by Message-ID.
- **UIDs are only valid within one UIDVALIDITY.** Anything persisted that refers to a
  remote UID also stores the folder and its UIDVALIDITY, and is checked before use.
- **Account-owned references** use composite `(id, account_id)` foreign keys; where that is
  impossible (`ON DELETE SET NULL`), the repository checks the account.
- **Secrets** never go into SQLite. Account creation writes the keyring entry before the
  row; removal deletes the row (`RETURNING secret_key`) before the keyring entry.
- **Blob GC** may only delete unreferenced blobs older than the grace period
  (see `BlobStore` docs). A new `*_blob_hash` column must be added to
  `Db::blob_references` (a test enumerates them), or GC deletes what it points to.
- **Reading never changes the server.** Sync and bodies use EXAMINE and `BODY.PEEK`
  only. Changes to messages go through the operation queue (`pending_ops`, replayed
  under SELECT after checking UIDVALIDITY), never straight to the server.
- **Outbox sends are never repeated behind the user's back.** `unknown` is committed
  *before* the final dot of SMTP DATA, with `write_durable` (a plain commit can be lost
  to a power cut), and only the user moves an entry out of it.
  Retries before that point are safe (the server discards an unfinished message);
  saving to Sent never resends. Keep the attempt count bumped before each APPEND, so a
  retry checks Sent first.
- **One engine per data directory**, enforced by the lock file. Don't add code paths that
  open the database without going through `MailClient`.

## Migrations

Migrations live in `crates/catmail-store/migrations/`. The project has not shipped yet,
so schema changes still edit `0001_init.sql` in place. Once a release exists, migrations
are append-only: never modify a released file, add a new one.

## Conventions

- Rust 2024 edition; `thiserror` in libraries, `anyhow` only in binaries; `tracing` for logs.
- Unit tests sit next to the code (`#[cfg(test)] mod tests`). A bug fix comes with a test
  that fails without the fix.
- Match the surrounding code's comment density and naming. Comments explain *why*.
- Pin new dependencies in the root `[workspace.dependencies]` and reference them with
  `.workspace = true`.
- Project documentation (README, `docs/`, notes) is written in Chinese; code, comments,
  commit messages and this file are in English.

## Change notes

Every **feature implementation, refactor, or bug fix** leaves a short Markdown record in
`.agents/note/`, committed together with the change it describes. Trivial edits (typos,
formatting, dependency bumps with no code change) don't need one.

**File name:** a short English summary title of the committed change, kebab-case, followed
by the date (`YYYY-MM-DD`, the day of the change):

```
.agents/note/<summary-title>-<YYYY-MM-DD>.md
.agents/note/split-messages-per-mailbox-2026-10-18.md
.agents/note/fix-orphaned-keyring-entry-on-account-removal-2026-10-21.md
```

If a file with that name already exists for the same day, extend the title rather than
overwrite it.

**Content** — brief, in Chinese, using this template:

```markdown
# <标题>

- 日期：YYYY-MM-DD
- 类型：功能 | 重构 | 修复

## 背景

为什么要做：需求、问题现象或触发原因（bug 写清楚复现条件）。

## 决策

做了哪些选择、考虑过但放弃的方案以及原因。

## 方案

改了什么、关键实现点、涉及的主要文件，以及如何验证（新增的测试、手动检查）。
```

Keep it to what a future reader needs to understand the change; the diff holds the
details. Read related notes in `.agents/note/` before working on an area that has them.
