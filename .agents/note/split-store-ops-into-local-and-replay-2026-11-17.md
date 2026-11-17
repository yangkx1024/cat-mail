# 把 store 的 ops.rs 拆成本地改动与回放簿记两部分

- 日期：2026-11-17
- 类型：重构

## 背景

`catmail-store/src/ops.rs` 到了约 2.7k 行，本地改动、入队、回放的记账和三十多个测试都在一个文件里，找一处要翻很久。
模块文档本身已经写明了两半：改动在本地立即生效并入队，回放取出操作、做完后回报结果。#320 第 3 项按这条边界拆开。

## 决策

- **`ops/local.rs`**：`set_flags` 到 `local`，即本地改动与入队（`carry_out`、`label_one`、`move_one`、`queue`、
  `insert`、`delete_on_server` 等），连同只有它用的 `Local`、`USER_FLAGS`。
- **`ops/replay.rs`**：`pending_folders` 到 `restore`，即回放的簿记（`pending`、`applied`、`moved`、`failed`、
  COPY 检查点、撤销、`forget_folder`、`end_generation` 等），连同回放的类型（`ReplayOp`、`ReplayKind`、
  `CopyCheckpoint`、`ReplayFolder`、`Moved`）和只有它用的 `WAITING`、`folder_of`、`supersede`、`gmail_label`。
  类型由 `mod.rs` `pub use` 出去，`crate::ops::ReplayOp` 等路径与 `lib.rs` 的再导出不变。
- **`ops/mod.rs`**：两边共用的 `Touched`、`MOVING`、`set_label`、`touch_shown`、`kind`，`PendingOps` 本身，
  以及列出、清除操作（`list`、`dismiss`、`has_settled`、`dismiss_settled`、`count_pending`，及其 `SETTLED`、
  `COLUMNS`、`from_row`）：它们既不是改动也不是回放，是用户看到的队列。
- `impl PendingOps` 分在三个文件里；私有方法、私有字段对子模块可见，所以可见性一处都没放宽。
- **测试跟着它测的代码走**：本地改动的 18 个进 `local.rs`，回放的 15 个进 `replay.rs`，清除的 2 个留在
  `mod.rs`；横跨两边的按测试名说的那一面分。共用的夹具（`Fixture`、`setup`、`folder`、`moved`、`Gmail`、
  `gmail`、测试用的 `ReplayKind::op_kind`）放在 `mod.rs` 的 `#[cfg(test)] mod fixtures`，各测试模块
  `use crate::ops::fixtures::*`，不重复。夹具里的项为此标了 `pub(super)`，只在测试里。
- 只搬动，不改行为。

## 方案

- `crates/catmail-store/src/ops.rs` → `crates/catmail-store/src/ops/{mod,local,replay}.rs`；
  `docs/architecture/op-queue.md`、`docs/architecture/labels.md` 里的路径。
- 验证：拆前拆后去掉 `use`、空行和 `pub(super)` 后的代码行按行计数比较，只差模块声明、`pub use`、
  `impl PendingOps<'_>` 的开头和结尾、`#[cfg(test)]`，以及 rustfmt 因 `pub(super)` 变长而折行的一个签名；
  catmail-store 测试数不变（141 个，其中 ops 35 个，名字一一对应）且全部通过，catmail-core 测试通过；
  fmt、`clippy --workspace --all-targets -D warnings` 通过。
