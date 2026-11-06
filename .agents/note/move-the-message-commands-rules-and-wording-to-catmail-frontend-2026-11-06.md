# 邮件命令的执行规则和失败文案移进 catmail-frontend

- 日期：2026-11-06
- 类型：重构

## 背景

#242：邮件命令的模型在 frontend，但"对哪些邮件执行、执行完怎么收尾"还在 gui：`keyed`（有勾选就对勾选的，否则对
选中的）、`shown_in`（邮件在哪个文件夹里看，决定 Gmail 上的移动会不会去掉当前标签）、`change`（只有被移走的
正是选中的那封才等它离开）。失败时的说法三种做法：邮件命令在 gui 里用 `what(n, 单数, 复数)` 拼了三遍，删除文件夹
和发件箱由 frontend 给 `what`。`Applies::of`、`archivable`、`deletion` 接收按账户找文件夹的闭包，是 GPUI 版用
`HashMap` 存文件夹留下的，只剩 `&[AccountEntry]` 一种调用方，每处都要经过 gui 的 `folders_in`。"在哪看"还有两种
算法：`shown_in` 按侧栏显示的文件夹，行上的标签名只认 `Source::Folder`。

## 决策

- 改动的结果带上失败时的说法：`Change { ids, what }`，`archivable`、`movable`、`deletion`（`Deletion.change`）、
  `toggled` / `mark_read`（`FlagChange`）、`label_change`（`LabelChange`）都给出它，和 `FolderDeletion::what`、
  `Choice::what` 一样由 gui 拼成"Couldn't {what}"。原来的 `what()` 去掉。打开即已读失败时说"mark the message as
  read"，比原来的"change the message"具体。
- `Applies::of`、`archivable`、`movable`、`move_targets`、`deletion` 都直接拿 `&[AccountEntry]`，删掉 gui 的
  `folders_in`；`movable` / `move_targets` 自己判断邮件是否同一账户。
- "在哪看"是 `Source::shown_in(account, accounts)`：All Inboxes 里是该账户的 INBOX，文件夹里是那个文件夹（属于该
  账户时），搜索、草稿、发件箱为空。只看列表的来源，不另传侧栏的 `Place`：两者只在搜索时不同，搜索时本来就为空。
  移动、"Move to"和行上的标签名都用它；All Inboxes 里行上的标签名因此会去掉 INBOX，INBOX 不是用户标签，显示不变。
- `keyed` 是 `Listing::keyed(checked, selected)`（另有 `Listing::checked`）；"只等选中的那封"是
  `ListModel::expect_removal(selected, ids)`，选中的不在其中时返回 `None`。
- 登录的"Couldn't sign in: …"仍由 `login::Submission` 拼整句：那里要把原因也说出来，不是"Couldn't {what}"这一种，
  issue 的建议也没有要求改它。
- 勾选只在归档、移动、删除时去掉，标签的改动不去掉，和原来一样。

## 方案

- `catmail-frontend`：`commands.rs`（`Change`、`FlagChange`、`LabelChange`、改为拿账户列表）和测试：归档只取会变的、
  说法随数量变、两个账户时不能移动；Gmail 上在标签里看时移动去掉它、"Move to"不列它所在的；永久删除的确认、没有
  Trash 时的模式、不认识的账户不算；开关和标签的增删。`mailbox.rs` 的 `Source::shown_in` 和表驱动测试；
  `list/model.rs` 的 `Listing::keyed` / `checked`、`ListModel::expect_removal` 和测试。
- `catmail-gui`：`mailbox/commands.rs` 只调 `MailClient`、显示通知（`run_change`）；`list/mod.rs` 的 `keyed`、
  `checked_messages`、`expect_removal` 交给模型，`is_searching` 换成 `source`；`rows.rs` 的标签名用 `shown_in`。
- gui.md 的读与改一节。
