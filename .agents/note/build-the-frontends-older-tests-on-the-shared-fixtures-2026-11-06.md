# catmail-frontend 的旧测试改用共用的 fixtures

- 日期：2026-11-06
- 类型：重构

## 背景

为移进 frontend 的模型加了 `fixtures.rs` 之后，`Folder`、`MessageSummary`、`OutboxEntry` 这些十几个字段的结构体
字面量在 crate 里出现了两三遍：folders.rs、outbox.rs、new_mail.rs 的测试各写一份。类型加一个字段，就要逐处去改。
精简时跳过了这项，因为它们的取值和 fixtures 不同，断言又依赖这些取值。

## 决策

- 不改断言：各测试保留自己的辅助函数和取值，只是改成 `..fixtures::x(..)` 再覆盖不同的字段。
  - folders.rs：`folder` 的分隔符由参数给，只有 INBOX 有角色；`with_role` 用 `.`；
  - outbox.rs：账户 2、身份 3；
  - new_mail.rs：有主题、发件人，日期就是 id。
- Gmail 上"All Mail、Trash、Spam 是位置，其余是标签"原来在 fixtures 和 folders.rs 测试里各写一遍，拆成
  `fixtures::on_gmail(folder)`，按文件夹自己的角色定 kind。folders.rs 的测试直接给角色，不靠名字推断，也用得上。

## 方案

`catmail-frontend` 的 `fixtures.rs`（`on_gmail`）和 folders.rs、outbox.rs、new_mail.rs 的测试辅助函数。测试内容和
断言不变，全部通过。
