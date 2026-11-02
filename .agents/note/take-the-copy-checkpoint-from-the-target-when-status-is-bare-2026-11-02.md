# STATUS 不给 UIDVALIDITY / UIDNEXT 时打开目标现取复制检查点

- 日期：2026-11-02
- 类型：修复

## 背景

issue #203。网易邮箱（Coremail）没有 MOVE，移动走 COPY + 删除原件；COPY 前 `ops::copy_move`
用 STATUS 取目标的 UIDVALIDITY 和 UIDNEXT 作检查点。163 对 STATUS 回空列表
（`* STATUS "INBOX" ()`），`Session::status` 报错，操作被当成服务器拒绝：163 上每次移动（包括删除到
已删除、归档）都失败，邮件回到原文件夹。

## 决策

- 最初按 issue 的方案退回到目标上次同步记下的 UIDVALIDITY 和 UIDNEXT（或最大 UID + 1）。review 指出
  这不安全：上次同步之后别的客户端复制进目标的同一封邮件（原文逐字节相同）落在检查点之后，COPY 前断线
  时 `find_copies` 会把它当成副本认领，原邮件被删、两封变一封（测试
  `an_identical_message_arriving_after_the_sync_is_not_taken_for_the_copy` 复现了）。“检查点偏小只是多搜
  几封”的判断是错的：检查点必须是 COPY 之前那一刻的。
- 改为 COPY 前打开目标现取：EXAMINE 给 UIDVALIDITY；EXAMINE 也不给 UIDNEXT 时用 `UID SEARCH UID *`
  取最大 UID + 1，然后重新 SELECT 原文件夹。只在 STATUS 缺值时多这几个来回。
- 放弃“退回值不可靠就当作无法判断”：那样网易上每次断线后的移动都会失败。
- 顺带解决了 #207：不再依赖本地同步状态，新建、没同步过的文件夹也能移入。
- EXAMINE 不给 UIDNEXT 导致同步从不记游标（没有新邮件通知、每次全量比对），是另一个问题，记在 #206。

## 方案

- `catmail-proto`：`Session::status` 返回 `UidStatus { uidvalidity, uidnext }`（都是 `Option`），不再
  因缺值报错；新增 `Session::last_uid`（`UID SEARCH UID *`）。
- `catmail-core/src/ops.rs`：新增 `copy_boundary`，STATUS 缺值时打开目标取检查点，再选回原文件夹
  （期间原文件夹被重建则 `copy_move` 返回 false，这个文件夹余下的 op 交给同步）。
- `catmail-testkit`：新故障 `bare_status`，STATUS 回空列表、EXAMINE/SELECT 不给 UIDNEXT。
- 文档：`docs/architecture/op-queue.md` 记下这条规则和不能用本地值的原因。
- 验证：`tests/ops.rs` 的 `moves_where_status_says_nothing`（目标为空和已有邮件、COPY 回答是否丢失，
  四种组合都只复制一份）、`an_identical_message_arriving_after_the_sync_is_not_taken_for_the_copy`（退回
  本地值的实现下失败）、`moves_into_a_new_folder_where_status_says_nothing`；`imap_session.rs` 检查空
  STATUS 返回 `UidStatus::default()` 和 `last_uid`。
