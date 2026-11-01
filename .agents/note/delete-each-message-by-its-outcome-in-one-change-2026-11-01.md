# 删除按每封邮件决定去向，一批删除是一次改动

- 日期：2026-11-01
- 类型：重构

## 背景

issue #190：`MailClient::delete_messages(ids, permanently)` 的 `permanently` 对整次调用生效，
`false` 时只要有一封邮件所在账户没有 Trash（`DeleteOutcome::NoTrash`）整次调用就报错。邮件列表多选时
（All Inboxes 或搜索结果里勾了不同账户的邮件），GUI 只能分两次调用：其他邮件用 `false`，没有 Trash 的用
`true`。两次各是一个写事务，第一次成功、第二次失败时只删了一部分，用户只看到一条"Couldn't delete the
messages"。

## 决策

- `permanently: bool` 换成 `DeleteMode`（`catmail-types`，挨着 `DeleteOutcome`），引擎逐封决定去向，与
  `MovePlan` 一样由引擎决定一个动作做什么：
  - `ToTrash`：原来 `false` 的语义，进 Trash、在 Trash 里的永久删除，遇到没有 Trash 的账户整批拒绝。CLI 不加
    `--permanent` 时用它：CLI 不向用户确认，"没有 Trash 的账户不会悄悄变成永久删除"仍由引擎保证。
  - `ByOutcome`：没有 Trash 的也永久删除，调用方已经向用户确认过。GUI 只在这批里有它已知没有 Trash 的账户的
    邮件（所以弹过确认）时用它，否则用 `ToTrash`。GUI 判断用的是缓存的文件夹列表，引擎提交时重新判断：
    Trash 在两者之间没了时，无条件用 `ByOutcome` 会把一次没确认的删除变成永久删除（code review 指出）。
  - `Permanent`：全部永久删除。
- 考虑过只留 `ByOutcome` / `Permanent`（issue 的建议），由 CLI 自己先逐封 `get_message` + `list_folders`
  检查 `NoTrash`。放弃：这在写事务外重复了引擎的判断、每封多读一次正文，以后不确认的前端还得记得照抄。

## 方案

- `catmail-store` `Ops::delete_messages` 接受 `DeleteMode`：`Permanent` 不看文件夹；否则按 `DeleteOutcome`，
  `NoTrash` 在 `ToTrash` 下报错、在 `ByOutcome` 下永久删除。`delete_on_server` 用 `Permanent`。
- `catmail-core` `MailClient::delete_messages` / `ops::delete_messages` 透传 `DeleteMode`。
- GUI `delete` / `delete_now`（`crates/catmail-gui/src/mailbox/commands.rs`）不再按 `NoTrash` 分组、分两次调用，
  按上面的规则选模式调一次；`DeleteOutcome::is_permanent` 仍决定要不要确认。
- CLI `rm` 按 `--permanent` 选 `Permanent` 或 `ToTrash`，行为不变。
- 文档：`docs/architecture/op-queue.md`、`gui.md`。
- 测试：store 新增 `deleting_decides_for_each_message`，一次 `ByOutcome` 调用里一封进 Trash、另一个没有 Trash
  的账户的邮件永久删除并入队 `delete`（修改前只能分两次调用）；同一批用 `ToTrash` 整批拒绝、什么都不改。
  GUI 新增 `deleting_is_never_for_good_unasked_when_trash_went_away`：引擎同步掉 Trash 后、窗口刷新前按 Delete，
  不弹确认，邮件留在服务器上（无条件 `ByOutcome` 时失败）。测试辅助 `TestMail::remove_mailbox_and_sync`。
