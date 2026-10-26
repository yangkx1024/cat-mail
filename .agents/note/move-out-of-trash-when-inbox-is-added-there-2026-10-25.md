# 给已删除 / 垃圾邮件里的邮件加 INBOX 按移到 INBOX 做

- 日期：2026-10-25
- 类型：修复

## 背景

#138。`label_messages` 只核对标签本身。对已删除里的邮件 `catmail label --add INBOX`，回放在已删除里发
`UID STORE +X-GM-LABELS (\Inbox)`：Gmail 立即把邮件移出已删除、回到 INBOX，本地这一行却还在已删除，等
下次同步才搬过去。GUI 的标签对话框只列用户标签，碰不到；CLI 和 `MailClient::label_messages` 可以。

用测试账户验证（每种情况一封新邮件，测完删掉）：已删除、垃圾邮件里 `+\Inbox` 都立即移出到 INBOX；加用户
标签、`\Important`，去用户标签，邮件都留在原处。只有 `\Inbox` 特殊。

## 决策

- 加 INBOX 按"移到 INBOX"做（`MovePlan::of` 的同一个计划：MOVE 到"所有邮件"，再加 `\Inbox`），而不是
  拒绝：服务器上本来就是这个效果，本地随之一致，也不用给接口加一种错误。规则写在 types 的
  `MovePlan::labelling`，与别的"动作做什么"在一处。
- 同一次调用里的其它标签改动在移动之后做（重新读一次邮件，它们等这次移动），与"移出已删除到某个标签"
  一致。
- 别的标签照常在原处改：验证过不会移动邮件。

## 方案

- types：`MovePlan::labelling` 及单元测试。
- store：`label_messages` 先按它移动，再改其余标签；测试 `inbox_added_in_trash_moves_the_message_out`。
- core：`MailClient::label_messages` 文档；假服务器测试 `inbox_added_in_trash_moves_the_message_out`
  （MOVE 到"所有邮件"再加标签，同步后行 id 不变）。两个测试在修复前都失败。
- 文档：`labels.md`（动作表、验证结果）、`cli.md`。
