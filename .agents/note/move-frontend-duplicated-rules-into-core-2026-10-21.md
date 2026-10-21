# 把前端重复实现的规则下沉到 core

- 日期：2026-10-21
- 类型：重构

## 背景

issue #36：CLI 和 GUI 各自重新实现了几块本该属于引擎的判断，以后的 IPC 前端会第三次复制。其中一处
已经和引擎不一致：GUI 按侧栏顺序找"那个 Trash"，store 按 id 最小找；账户有两个 Trash 时，在引擎
移入的那个 Trash 里按 Delete，GUI 以为不是永久删除而不确认，引擎却因为它在 Trash 里而永久删除。

## 决策

- 规则放在 `catmail-types`，是对文件夹列表的纯函数：GUI 渲染工具栏时要同步知道"能否归档""删除是否
  永久"，异步的 `MailClient` 查询做不到。store 也调用它们：`by_role` 先 `list` 再 `special_folder`，
  删除和归档按账户取一次文件夹列表，不再有一份平行的 SQL 规则。
- `DeleteOutcome` 有三种（`ToTrash` / `Permanent` / `NoTrash`）：store 对"没有 Trash 又没要求永久"
  仍报错，不悄悄永久删除；GUI 对后两者都先确认。任一 Trash 角色的文件夹都算在 Trash 里，归档同理。
- 核心加了 `archive_messages`，CLI 随之有了 `catmail archive`；GUI 的归档改用它。
- outbox 的可用操作是 `OutboxStatus` 上的谓词，store 的 retry / mark_sent / cancel 也用它们判断冲突；
  重试后的状态由 `retry_to` 给出（`can_retry` 由它推出），store 不再有第二份 `match`。按钮措辞和顺序
  留在 GUI（`OutboxChoice`）。考虑过一整张状态转换表（操作 × 状态 → 结果），但 Cancel 的结果是删除
  而不是状态、mark_sent 有副作用、Queued 的"Send now"是界面的选择，收益不抵复杂度，暂不做。
- 显示帮助函数放进 `catmail_core::display`（要用 jiff 取本地时区，types 不该依赖它）。GUI 的
  `format` 重新导出它们，只保留自己的时刻与日期分组。CLI 的 ROLE 列仍用 `as_str`（是可输入的标识）。
- 角色名和是否显示未读数在 types（`label` / `counts_unread`），图标和顺序是 GUI 的事，收进
  `folders::RoleInfo` 一张表。GUI 的归档、移动、删除共用一个 `remove` 帮助函数。
- 五个确认框合成 `layout::confirm_danger`。"Send again" 因此也用红色按钮：可能让收件人收到两次，
  和其他不可撤销的操作同类。

## 方案

- types：`folder.rs` 的 `special_folder`、`DeleteOutcome`、`archive_folder`、
  `FolderRole::label` / `counts_unread`；`outbox.rs` 的 `went_out`、`needs_user`、`retry_to` / `can_retry`、
  `can_mark_sent`、`can_cancel`、`retry_may_duplicate`。
- store：`folders.rs` 的 `by_role` 用 `special_folder`；`ops.rs` 的 `delete_messages` 用 `DeleteOutcome`，
  新增 `archive_messages`；`outbox.rs` 用谓词。
- core：`display.rs`（回复、转发的日期和地址也用它）、`MailClient::archive_messages`。
- CLI：去掉 `output::date/size`、`subject`、`recipients`、`addresses`、`went_out`，新增 `archive`。
- GUI：`commands.rs` 的归档 / 删除、`folders.rs` 的 `RoleInfo` 与扁平文件夹列表、`outbox.rs` 的
  `outbox_choices`、`html.rs` 的 `Carried`（`image_source` 与"远程图片未加载"共用）、
  `layout::confirm_danger`。
- 测试：types 的特殊文件夹、删除与归档规则；store 的归档；
  GUI 的 outbox 选择表，以及 `deleting_from_trash_asks_first_whichever_trash_it_is`（两个 Trash，
  恢复旧判断时失败）。
