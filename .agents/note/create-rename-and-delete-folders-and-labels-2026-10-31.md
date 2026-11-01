# 新建、重命名、删除文件夹与标签

- 日期：2026-10-31
- 类型：功能

## 背景

侧栏只显示文件夹和 Gmail 标签，没有办法新建、重命名或删除它们。

## 决策

- **当场在服务器上做，不进操作队列。** 只在本地的文件夹没有 UIDVALIDITY，移进去的邮件无从绑定；名字被占用等
  拒绝只有服务器知道，排队之后才拒绝会让用户事后收到失败。代价是要在线，文件夹改动很少，可以接受。
- 改完照常 `LIST` 并对账（抽出 `sync::store_folders`，同步也用它），本地以服务器为准。
- **重命名先在本地改名再对账**（同一事务），邮件、正文、待回放的操作都留着；否则对账会把它当成删了一个、
  新出现一个，邮件全部重拉，待回放的操作丢失。
- **删除前先回放操作队列**，删除的文件夹里仍有待回放的操作就不删：否则本地已经移走的邮件会随文件夹一起被删。
  子文件夹由深到浅逐个删除。Gmail 标签要在对账之前显式删除，否则对账把仍被邮件带着的标签留作隐藏的标签。
- **答复丢失后重来时先 LIST**，已经做成的不再发命令，避免把做成的操作报成失败。
- Codex review 后补上的并发与边界（每条都有测试）：这次回放里移出被拒绝时不删；删除期间引擎记下这些文件夹，
  本地改动在写事务里检查、碰到就拒绝（删除查看队列也放进写事务，两者之间没有空隙）。只看真正改动的文件夹
  （所在、移入、加减的标签），不看只是显示邮件的标签：`Touched` 拆出 `shown`，否则 Gmail 上删标签期间给带着它的
  邮件加星也被拒绝；也不能改成只看操作所在的文件夹，那样会放过移入和加标签；文件夹、名字、层级在拿到
  同步锁之后才读；删除按服务器当前的 LIST 找子文件夹；名字按父文件夹（或 INBOX）的分隔符检查。
- 有角色的文件夹、包含它们的（`[Gmail]`）和隐藏的标签不能改，规则在 types 的 `can_change_folder`，GUI 用它决定菜单项。
- 在哪里能建子文件夹（`can_create_in`）：服务器能事先说的只有 `\Noinferiors` 和没有分隔符，都读出来（`folders.inferiors`）；
  但 Dovecot、Gmail 不给 INBOX 标 `\Noinferiors`，所以另加规则：特殊文件夹及包含它们的层级里不能建，归档除外
  （`Archive/2026`）。引擎和 GUI 菜单共用，只看服务器信号的方案被放弃（INBOX 的菜单上仍会出现 New folder）。
- 只在 INBOX 下有文件夹的服务器（Courier、Cyrus 的 `INBOX.` 前缀）上，"最上层"建在 INBOX 下。考虑过 NAMESPACE，
  但 imap-proto 不解析它，暂用推断。
- 重命名只改最后一层，不支持移到别的父文件夹下。
- GUI 用右键菜单（账户地址、文件夹、标签），另有 F2 重命名选中的文件夹。New folder / New label 总是建在右键点的
  地方（地址上是最上层，文件夹上是它里面，不允许就不显示）；起初文件夹上同时有建在最上层的 New folder 和
  New subfolder，两者建在哪里有歧义，改成只有一种。菜单项的命令推迟到菜单关闭后执行：
  否则对话框把焦点还给已关闭的菜单，窗口的快捷键全部失效（测试里 F2 因此失灵才发现）。

## 方案

- proto：`Session::create` / `rename` / `delete`，附带 SUBSCRIBE / UNSUBSCRIBE；改名、删除选中的邮箱前先 EXAMINE INBOX。
- testkit：假服务器支持这些命令（Gmail 模式下改邮件上的标签），故障 `drop_after_mailbox_change`，`mailbox_names`；
  连接只按名字记选中的邮箱，邮箱被改名或删除后 BYE（原来按下标，删一个邮箱会让别的连接指向错的邮箱）。
- store：`Folders::remove`（从 `reconcile` 抽出，另外为标签通知显示其邮件的列表）、`rename`、`remote_name`。
- types：`can_change_folder`、`Folder::leaf_name`、`is_above`。
- core：`folders.rs`，`MailClient::create_folder` / `rename_folder` / `delete_folder`。
- CLI：`catmail folder add / rename / rm`；`find_folder` 移到 `cmd/mod.rs` 与 `mv` / `label` 共用。
- GUI：`mailbox/folder_commands.rs`（右键菜单、命名对话框、删除确认），`RenameFolder` 动作（F2）；`Submit` 可以说明是什么失败了；
  表单对话框抽成 `submission::open_dialog`（与添加发件身份共用；`Submit` 也从 `login_form` 移进 `submission.rs`），菜单打开对话框的焦点问题抽成 `layout::after_menu`。
- 文档：新增 `docs/architecture/folders.md`，更新 architecture、gui、cli、README。
- 验证：store、types、core 单测；`crates/catmail-core/tests/folders.rs`（含 Gmail、答复丢失、先回放移动）；
  `tests/dovecot.rs` 对真实 Dovecot（新用户 `folders`）；CLI 单测；GUI 测试 `managing_folders`（含各处菜单有哪些项、在归档里新建）；截图检查菜单与对话框。
