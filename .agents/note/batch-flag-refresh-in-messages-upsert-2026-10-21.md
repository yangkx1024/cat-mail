# Messages::upsert 批量刷新已存在行的标记

- 日期：2026-10-21
- 类型：重构

## 背景

Issue #33：同步中断后续传时，`Messages::upsert` 对每个已存在的 UID 单独调用
`update_flags`，而 `update_flags` 每次都重新查询该文件夹的 `pending_ops`。一批 200 封
已存在的行就多出 200 次查询。

## 决策

插入循环中只收集已存在行的 `FlagState`，循环结束后调用一次 `update_flags`。延后刷新不改变
结果：`update_flags` 只更新已存储的行，同一批里重复的 UID 依旧先插入、后刷新。
`FlagState` 的克隆开销远小于一次 SQL 查询，因此没有为此把 `update_flags` 改成泛型参数。

## 方案

- `crates/catmail-store/src/messages.rs`：`upsert` 收集已存在行，每批调用一次 `update_flags`。
- `crates/catmail-store/src/ops.rs`：新增 `repeated_batch_keeps_unreplayed_flag_changes`，
  验证重复批次经批量刷新后仍保留未回放的本地标记修改。查询次数本身没有测试（rusqlite 未开启
  `trace`），行为由现有及新增测试覆盖。
