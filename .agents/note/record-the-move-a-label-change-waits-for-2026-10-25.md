# 显式记下标签改动等的那条移动（after_op）

- 日期：2026-10-25
- 类型：重构

## 背景

#134。#128 让"在一次移动之后做的标签改动"等这次移动，但这个依赖没有存下来，而是推出来的：同一个
（文件夹, UIDVALIDITY, UID）上有 id 更小、还在 pending 的 move。推导写在两处：`WAITING`（`pending` /
`pending_folders` 排除等待中的标签改动）和 `fail_waiting`（那条 move 已标为 failed，只能另写一条查询）。
以后别的改动要等移动还得再推一遍，两处也可能改得不一致。

## 决策

- `pending_ops` 加 `after_op`（`REFERENCES pending_ops(id) ON DELETE SET NULL`）。移动做成后被删除，
  这一列随之为空；`AUTOINCREMENT` 保证 id 不复用。SET NULL 用不了复合外键，只在同一封邮件的 move 上设，
  所以同一账户。
- 等待条件是"`after_op` 指向的 op 仍是 pending"。move 被改写成 delete（移动途中永久删除）时在改写处
  清空这一列：不再等，与原来一致。考虑过在查询里再判断 `kind = 'move'`，但那等于把"删除不用等"藏在
  回放的查询里，以后每个读这一列的地方都得重复。
- 哪些改动要等由 `queue` 一处决定：标签改动取邮件的 `Local::moving`，标记改动不等（在移动之前、原位置
  回放）。`carry_out` 移动之后重新读一次 `local`，而不是手工拼出移动后的 `Local`（多一次按主键的查询，
  换来不重复 `local` 的推导）；`label_one` 因此不再需要单独的文件夹参数。
- `fail_waiting` 只按 `after_op` 找，不再需要文件夹和位置。
- 没做：`fail_waiting` 仍只处理标签改动。现在只有标签改动会等，把 `failed` 里按种类撤销的部分抽出来
  给它共用，等真的有第二种再做。

## 方案

- `0001_init.sql`：`after_op` 列、`pending_ops_after` 索引（SET NULL 删除父行时要查子表）和注释。
- `catmail-store/src/ops.rs`：`WAITING`、`queue` / `insert` 记 `after_op`、`delete_messages` 改写移动时清空它、
  `carry_out` / `label_one`、`fail_waiting`。
- `docs/architecture/op-queue.md`："回放 / 顺序"说明依赖记在哪里。
- 验证：现有测试（`a_label_change_waits_for_the_move_out_of_trash`、`deleted_and_moved_back_to_inbox_it_keeps_its_row`
  等）照常通过；新增 `a_label_change_waits_no_more_once_its_move_turns_into_a_delete`。
