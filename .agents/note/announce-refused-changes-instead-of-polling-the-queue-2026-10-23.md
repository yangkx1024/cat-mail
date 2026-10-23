# 改动被拒绝时单独发事件，前端不再每次都查队列

- 日期：2026-10-23
- 类型：重构

## 背景

Issue #63。`PendingOpsChanged` 在队列有任何变化时都发，不说明是哪种变化。GUI 为了提示"服务器拒绝了
某个改动"，每收到一次就调一次 `dismiss_settled`：标记已读这类最频繁的操作每次都多一次 runtime 往返和
一次读事务，离线时队列越长越贵，`dismiss_settled` 自己发的 `PendingOpsChanged` 还要再引来一次查询。

## 决策

- 新增 `Event::ChangesRefused { account }`，而不是给 `PendingOpsChanged` 加字段：语义清楚，CLI 也能用
  （`catmail watch` 打一行提示）。它和 `PendingOpsChanged` 一起发，只在 op 变成失败或冲突时。
- 信息由 store 带出：只有 `PendingOps::failed` 和 `conflict` 把 op 标成非 pending。把 `Touched` 从
  `BTreeSet<(AccountId, FolderId)>` 改成结构体，加 `refused` 账户集合，这样经 `moved`、
  `reset_uidvalidity`、`reconcile`（`\Noselect`）传上来的冲突自动跟着走，不用在每条调用链上另加返回值。
  合并两个 `Touched` 只用 `merge`；`Touched` 不实现 `IntoIterator` / `Extend`，加文件夹写明
  `touched.folders`，免得合并时悄悄丢掉 `refused`。
- 删除文件夹时它的 op 级联删除，不算冲突，不发事件。
- GUI 只在 `ChangesRefused`、窗口打开时和漏收事件后调 `dismiss_settled`；之前靠首次同步的
  `PendingOpsChanged` 顺带查出遗留的失败，现在改为打开时显式查一次。

## 方案

- `catmail-store/src/ops.rs`：`Touched` 结构体；`failed` 的 `RETURNING` 带上 `account_id`，`conflict`
  记录每行的账户。
- `catmail-core/src/ops.rs`：`announce` / `replayed` 遍历 `touched.folders`，最后为 `touched.refused`
  发 `ChangesRefused`（提交后、在做改动的引擎任务里）。
- `catmail-types` 新事件；CLI 的 JSON 事件和 `watch` 描述；GUI `refresh.rs`、`mailbox/mod.rs`。
- 测试：store 单测断言拒绝 / 新代际报告账户、目标文件夹重建不报告；假服务器测试
  `refused_changes_are_undone`（注入 `refuse_changes`：本地改动不发、回放发一次、dismiss 不发）和
  `a_rebuilt_folder_turns_changes_into_conflicts`（UIDVALIDITY 变化发一次）；GUI 测试
  `a_change_refused_before_the_window_opened_is_reported`（去掉打开时的查询即失败）。
