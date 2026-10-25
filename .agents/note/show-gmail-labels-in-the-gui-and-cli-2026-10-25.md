# GUI 与 CLI 显示 Gmail 的标签

- 日期：2026-10-25
- 类型：功能

## 背景

[位置与标签](../../docs/architecture/labels.md)迁移步骤 6（#129）。#126–#128 之后 Gmail 一封邮件一行、
移动和归档改的是标签，但界面仍把标签当文件夹列在"其余文件夹"里：分不出标签和位置，邮件看不到自己的标签，
也没有加 / 去标签的入口；在 IMAP 里隐藏的标签（`listed = 0`）照样列出；"所有邮件"隐藏的账户没有任何提示。
CLI 能改标签（#128 的 `label`、`mv --from`），但同样看不到标签，`--json` 也没有。#129 只写了 GUI，CLI 是同级
的前端，放在同一个分支里一起补上。

## 决策

- **侧栏**：系统标签有角色，与位置一起按角色排在前面（不变）；用户标签单列一节"Labels"（gpui-kit 的
  `SidebarGroup`，与其他小节同样的淡色小标题），图标是标签。只在 Gmail 上有这一节，普通账户不加"Folders"
  之类的标题：那里没有要区分的东西。
- **邮件带上标签 id**（`MessageSummary.labels: Vec<FolderId>`），名字由前端按账户的文件夹查：types 只放数据，
  显示哪些（用户标签、`listed`）是前端的事。查询在 `SUMMARY_COLUMNS` 里用 `group_concat` 子查询，按
  `message_labels` 的主键一次查找。考虑过在 store 里直接给出名字，但那就把"只显示用户标签"写进了存储层。
- **列表的行**在主题后面显示中性的小标签，最多两个、其余 `+N`，不显示正在看的那个标签（每一行都有它）。
  **读信区**在主题下面全部显示。读信区的标签不从打开时读的 `Message` 来，而由 `Mailbox` 观察列表、按列表里
  这封邮件的现状设置：在对话框里改标签时，打开的邮件随之变化。
- **加 / 去标签**：复用"Move to"的对话框（`move_to.rs` 改名为 `folder_picker.rs`），多一种勾选的模式；点一个
  立即生效，对话框留着可以继续点。没有做"选完再应用"：每一步的结果在背后的列表和读信区里立即可见，少一个
  按钮。快捷键 L（Gmail 的键），只在消息列表里。普通账户不显示按钮（不是禁用：那里没有标签这个概念）。
- **"用户标签"的规则放在 types**（`Folder::is_user_label`，含 `listed`：隐藏的标签哪里都不显示），GUI 和 CLI 共用。CLI 显示的是文件夹的完整名字
  （`Work/Clients`），与 `catmail label --add` 收的名字一致；`--json` 的 `labels` 给全部标签的 id（系统标签也在内），
  由脚本按 `ls --json` 的文件夹对应，只增加字段，`schema_version` 不变。
- **CLI 的列按需出现**：KIND 只在账户有标签时、LABELS 只在有邮件带用户标签时出现，普通账户的输出不变。
- **"Labels"对话框由 `Mailbox` 控制**：最初对话框自己翻转勾选、记下打开时的邮件 id。codex 评审指出两处问题：
  在标签里去掉这个标签后列表打开了下一封，对话框仍开着、继续改原来那封；回放被拒绝撤销（或同步拉到别处的
  改动）后勾选与邮件不一致，再点会做相反的事。改为点击只报告点了哪个标签，加还是去按邮件现在的标签定；
  `Mailbox` 观察列表，每次变化按这封邮件现在的标签重设勾选，它不再是选中的那封时关掉对话框
  （`sync_labels_dialog`）。考虑过让对话框跟到新选中的邮件，但用户刚为某封邮件打开它，换目标不如关掉明白。
- **"所有邮件"隐藏**：账户上记下服务器是不是 Gmail（`accounts.gmail_server`，对账文件夹时按 `X-GM-EXT-1` 写入），
  "隐藏"由它和模式推出（`Account::all_mail_hidden()`：Gmail 服务器、普通模式）。最初直接存了 `all_mail_hidden`，
  评审指出它只在普通模式下有意义、能表达"Gmail 模式却隐藏"这种不可能的状态；也考虑过给 `AccountMode` 加第三个
  值，但模式决定数据怎样建模，"隐藏了所有邮件的 Gmail"建模上就是普通账户，加了值就要在 `set_mode` 的"模式变了
  就重建"里特判。存服务器的事实没有这两个问题，#135（按身份记下是否自动存已发送）的默认值也可以基于它，
  而不是账户模式。提示放在侧栏账户名下面一行，常驻而不是通知：问题一直在，直到用户改设置。
- **修了两处随之暴露的问题**：
  - 标签变了只通知变了的那个标签和邮件的位置，没有通知列出这封邮件的其他标签；行里不显示标签时没人在意，
    现在在 INBOX 里给邮件加 Work，INBOX 的列表不刷新。本地改动（及回放被拒绝时的撤销）和同步拉到的都是
    这样：本地的在 `label_messages` / `carry_out` 里每封邮件 `touch_shown` 一次（不是每个标签一次），同步的
    在 `Messages::set_labels` 里有变化就 `note_shown`。
  - `\Noselect` 的父级（Gmail 的 `[Gmail]`）在子文件夹都提到角色一节后，作为空行留在侧栏；原来只去掉服务器
    没列出的占位父级，现在凡是不能存邮件、又没有子节点的都去掉。

## 方案

- types：`MessageSummary.labels`、`Account.gmail_server` 与 `all_mail_hidden()`。
- store：`0001_init.sql` 加 `accounts.gmail_server`；`Accounts::set_gmail_server`；`SUMMARY_COLUMNS`
  带标签；`ops.rs` 的 `label_one` 和标签的撤销 `touch_shown`。
- core：`sync::run` 对账时记下 `gmail_server`，变化时发 `FoldersChanged`。
- GUI：`folders.rs`（`AccountFolders.labels`、`user_labels`、`icon`，去掉空的 `\Noselect` 父级）、`sidebar.rs`
  （"Labels"一节、账户说明）、`message_list`（`set_labels`、`label_names`，行里的小标签）、`reader`（主题下的
  标签）、`folder_picker.rs`（"Labels"模式）、`commands.rs`（`edit_labels`、`label_message`）、`panes.rs`
  （工具栏按钮）、`actions.rs`（`EditLabels`、L 键）；Heroicons 的 `tag`（2.2.0，与已有的同一版本）。
- CLI：`output.rs`（`folder_table`、`message_table` 拆成纯函数以便测试，KIND / LABELS 列，`read` 的 `Labels:`，
  `ALL_MAIL_HIDDEN`）、`cmd/mod.rs`（`account_folders`）、`ls`、`search`、`read`、`sync`（同步后重读账户）、
  `json.rs`（`kind`、`listed`、`labels`、`mode`、`gmail_server`、`all_mail_hidden`）。
- 文档：`labels.md`（状态、界面一节、步骤 6）、`gui.md`、`cli.md`、`architecture.md`。
- 验证：store 的 `a_label_change_shows_in_every_label_listing_the_message` 和
  `labels_show_and_count_what_is_in_all_mail` 里同步加标签的断言（修复前都失败），以及列表带标签的断言；
  `accounts` 的 `gmail_server`；core `tests/gmail.rs` 隐藏 / 恢复"所有邮件"时的 `gmail_server` 与 `all_mail_hidden()`；GUI 的
  `folders` 单元测试（Gmail 的分节、`[Gmail]` 去掉、隐藏的标签不列）；`tests/labels.rs` 用假 Gmail 服务器驱动
  窗口：侧栏分节与邮件的标签、对话框加 / 去标签并回放到服务器、在标签里去掉它后打开下一封、隐藏"所有邮件"
  时的说明且没有标签按钮、去掉当前标签后对话框随邮件关掉、被拒绝的改动撤销后勾选也撤销（后两者在修复前
  失败）；CLI 的 `folders_tell_labels_apart_and_leave_hidden_ones_out`、
  `messages_show_their_user_labels` 和 JSON 快照（`folder_snapshot`，`message_snapshot` 带上标签）；截图（`gmail`、`gmail-labels`、`gmail-all-mail-hidden`）人工看过。
