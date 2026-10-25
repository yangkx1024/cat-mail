# 移动途中永久删除时丢掉等这次移动的标签改动

- 日期：2026-10-25
- 类型：修复

## 背景

Gmail 上把已删除里的邮件移回 INBOX（MOVE 到"所有邮件"，再加 `\Inbox`，后者等这次移动），回放之前又
永久删除：移动改写成删除，等它的 `+\Inbox` 不再等，在已删除里先于删除回放。用测试账户验证（脚本发与
回放相同的命令）：已删除里 `UID STORE +X-GM-LABELS (\Inbox)` 让 Gmail 立即把邮件移出已删除（已删除里
EXPUNGE，回到 INBOX），紧接着的 `\Deleted` + `UID EXPUNGE` 删不到它，用户永久删除的邮件留在 INBOX。
不加这一步时同样的删除把它彻底删掉。#134 的 /simplify 评审发现。

## 决策

- 移动改写成删除时，等这次移动（`after_op`）的 op 一起删掉：它们是冲着新位置去的，邮件到不了那里了，
  本地这一行也随删除没了，没有要撤销的。
- 考虑过只不让 `+\Inbox` 在已删除里回放（按标签、按文件夹判断），但这是特例；等移动的改动在移动取消
  之后本来就没有意义，一并丢掉更简单。
- 直接在已删除里给邮件加 `\Inbox`（`catmail label --add INBOX` 作用于已删除里的邮件）同样会把它移出
  已删除，本地却仍在已删除，是另一个问题，不在这次修复里。

## 方案

- `catmail-store/src/ops.rs`：`delete_messages` 改写移动时 `DELETE FROM pending_ops WHERE after_op = ?1`。
- 文档：`labels.md`（验证结果、不在已删除里加 `\Inbox`）、`op-queue.md`、`0001_init.sql` 注释。
- 验证：store 测试 `label_changes_waiting_for_a_move_go_when_it_turns_into_a_delete`、假服务器测试
  `a_restore_out_of_trash_deleted_for_good_in_time_is_deleted_there`（不发 `X-GM-LABELS`、在已删除里
  EXPUNGE），修复前都失败；真实 Gmail 行为见上，测试邮件已删除。
