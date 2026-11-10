# 自己跑 schema 迁移，去掉 rusqlite_migration，升级 rusqlite 0.41

- 日期：2026-11-10
- 类型：重构

## 背景

`cargo deny` 因 RUSTSEC-2026-0459（CVE-2026-119375）失败：libsqlite3-sys 0.38.2 打包的 SQLite 3.53.2
在库文件与旁边的 `-wal` 页大小不同时会读出未初始化的内存。修复在 libsqlite3-sys 0.39（SQLite 3.54），
随 rusqlite 0.41 发布；0.38 不再出修复版。rusqlite_migration 2.6.0 要求 `rusqlite ^0.40`，上游升级的
PR（cljoly/rusqlite_migration#370）还没合，也没发版，挡住了升级。

## 决策

- 去掉 rusqlite_migration，在 store 里写一个小的迁移函数。我们只用了它的 `M::up`、`to_latest`、
  `validate`；它不处理的难点（重建表、事务外关外键、进度、备份）本来就要自己做。以后 SQLite 的安全
  更新不用再等它。
- 考虑过在 `deny.toml` 里暂时忽略这条 advisory（我们只打开自己数据目录的库，实际风险低），等上游
  发版：时间不确定，`cargo deny` 期间一直是红的。用 `[patch.crates-io]` 指向 #370 的分支：要把仓库
  放进 `allow-git`，钉在 dependabot 分支上，不取。
- 计数与 rusqlite_migration 相同（`user_version` 是跑过的迁移个数），现有的库不用动，以后也能换回去。
- 比原来多两项检查：库比程序新时拒绝（`Error::SchemaTooNew`），提交前 `PRAGMA foreign_key_check`
  （重建表时外键在事务里关不掉）。不做反向迁移、Rust 写的数据迁移，用到时再加。

## 方案

- `crates/catmail-store/src/migrate.rs`：`to_latest`，一个 IMMEDIATE 事务里读 `user_version`、跑没跑过的
  迁移、检查外键、写回版本号。`lib.rs` 的 `MIGRATIONS` 改成 `&[&str]`；`error.rs` 的 `Migration` 改成
  带文字，新增 `SchemaTooNew`。
- rusqlite 0.40 → 0.41.1，libsqlite3-sys 0.38.2 → 0.39.1，其余代码不用改。
- `docs/architecture/storage.md` 记下迁移的规则和与 rusqlite_migration 计数一致这条约束。
- 测试（`migrate::tests`）：新库跑全部、已迁移的不重跑、从中间版本（如 rusqlite_migration 留下的库）
  只跑剩下的、较新的库拒绝且不动、失败整个回滚、弄坏外键回滚、负的版本号拒绝；`migrations_are_valid`
  改为在内存库上跑一遍 `MIGRATIONS`。全量测试、clippy、fmt 通过（macOS）。
