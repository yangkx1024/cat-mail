# 侧栏加入 All Inboxes

- 日期：2026-10-30
- 类型：功能

## 背景

多个账户时要逐个点开各自的 Inbox 才能看到新邮件。需要一个集中展示所有账户收件箱的位置，放在左栏
"New message"下面，打开应用时默认选中它；只有一个账户时它与那个账户的 Inbox 相同，不显示。

## 决策

- 合并在 store 里做（`Messages::list_merged`）：每个收件箱用原来的 `list` 取游标之后的 `limit` 封，归并后
  截到 `limit`。前 n 封必在各自的前 n 封里，游标 `(date, id)` 全局有序，所以分页与单个文件夹完全一致，
  不另写跨文件夹的 SQL（Gmail 的 INBOX 是标签，走 `message_labels` 的索引，两种查询形状不同）。
- 哪个是收件箱沿用核心已有的 `inboxes`（`special_folder(Inbox)`），与新邮件通知、托盘未读数一致。
- GUI 加 `Place::Inboxes` / `Source::Inboxes`，不按账户挑选具体文件夹。它是否存在由 `Place::is_in` 一处决定
  （账户多于一个），侧栏、默认选中、"原来的位置还在不在"都用它：删到只剩一个账户时回到那个账户的 Inbox。
- 选中位置显示哪些文件夹由 `Mailbox::shown_folders` 一处回答（一个文件夹，或 All Inboxes 的各收件箱），
  移动时的 `shown_in`、打开时同步、未读数、通知落在哪里、刷新都用它，不各自特判 All Inboxes。
- 刷新：只有显示中的收件箱 `MessagesChanged` 才重读 All Inboxes（不像搜索那样任何文件夹都重读：同步
  其他文件夹时不白读）；账户 / 文件夹增减时也重读，收件箱可能变了。
- `list_merged` 每个收件箱读满一页再截断，N 个账户读 N 倍的行；账户数通常很少、在引擎线程上读，
  没有做逐行归并。
- Gmail 上的移动要知道"在哪个标签里看的"：在 All Inboxes 里看作在邮件所属账户的 INBOX 里（`shown_in`
  改为按邮件取），否则移到别的标签不会去掉 INBOX，邮件留在 All Inboxes。
- 新邮件通知：窗口正显示 All Inboxes 时就在这里打开那封信，不跳到账户的 Inbox；新开的窗口仍去账户的 Inbox。
- 写新邮件在 All Inboxes 时用第一个账户（与没有选中位置时相同）。
- 图标用 Heroicons 的 inbox-stack；行里不显示账户（同搜索结果，不加多余标签）。

## 方案

- `catmail-store/src/messages.rs`：`list_merged` 及分页测试（跨两个账户、缺日期、同一文件夹给两次）。
- `catmail-core/src/client.rs`：`MailClient::list_inboxes`；`tests/gmail.rs` 测 Gmail 的 INBOX 标签与普通账户的
  INBOX 合并、分页、归档后离开。
- `catmail-gui`：`mailbox/mod.rs`（`Place::Inboxes`、默认选中、`inboxes()`、选中时同步各收件箱）、
  `sidebar.rs`（侧栏第一节，未读之和）、`message_list`、`refresh.rs`、`commands.rs`（`shown_in`）、`panes.rs`
  （标题和未读数）、`assets.rs`。测试：`labels.rs` 新增从 All Inboxes 移动 Gmail 邮件会去掉 INBOX（去掉修复时
  失败）；`reading.rs` 新增多个账户时默认选中 All Inboxes、删到一个账户时回到 Inbox；`resident.rs` 新增
  显示 All Inboxes 时点击通知在这里打开。`TestMail::add_another_account` 在同一服务器上加第二个账户。
- 文档：`docs/architecture/gui.md` 侧栏与刷新、README。
