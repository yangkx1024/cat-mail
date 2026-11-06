# 侧栏的模型（分组排序、重读后的规则）移进 catmail-frontend

- 日期：2026-11-06
- 类型：重构

## 背景

#243：`mailbox/sidebar.rs` 除了画界面，还有分组排序（`Group`、`Heading`、`Item`、`groups`、`places`、
`folder_item`）和读回账户之后的规则（草稿数取哪次的、第一次读到时去哪、原来的位置不在了、列表要不要重读）。
排序只有一个要编译 GTK 和 nsh 才能跑的测试，重读后的规则没有测试；gui.md 说侧栏的模型在 frontend，和现状不符。
分组在每一层都克隆整棵子树，账户每变一次就复制一次。

## 决策

- 新模块 `catmail_frontend::mailbox::sidebar`：
  - `sidebar_groups(accounts, &Folds)`：`Folds` 是普通类型，合并了原来的账户折叠和文件夹折叠两个 signal；
    `CATMAIL_GUI_UNFOLD` 由 gui 读，设到 `Folds::unfold_all`，frontend 不读环境变量。
  - `Group.items` / `labels`、`Item.children` 改成 `Rc<[Item]>`：nsh 的 `each` 要 `Vec`，各层只复制这一层的行，
    子树共享。
  - `SidebarModel`：一次一个读取（`start_read` 给出带着当时草稿数的 `Read`，`finish_read` 说要不要再读）、草稿数
    （`drafts_changed`、`is_latest_count`）、窗口打开前要求去的位置和写信（`go_to`、`compose`，第一次读取时取走）。
    `loaded(read, overviews, status, shown, place)` 返回 `Loaded`：要显示的账户、选中去哪、要不要重读列表、要
    reveal 的邮件、要不要写信；和正显示的一样时返回 `None`。
- 和 issue 的建议不同，模型**不持有** accounts 和 place：它们仍是 gui 的 signal（列表、命令、写信窗口、设置都在读），
  由调用方传进来。模型再存一份就要在两边同步，或者每次更新一个账户都整份复制。模型只持有不需要显示的状态。
- `update_account`（连接状态、单个账户的草稿数）仍在 gui：只是改 signal 里的一项。

## 方案

- `catmail-frontend`：`mailbox/sidebar.rs`（模型和测试：账户内的顺序、All Inboxes 和多账户折叠、未列出的层级的
  key、第一次读取去哪、重读后保留或回到开始处、All Inboxes 多一个收件箱时重读列表、更新的草稿数保留、读取合并）。
- `catmail-gui`：`mailbox/sidebar.rs` 去掉 `Waiting`、`Folds`、`FolderFolds`、`ReadState` 和分组的代码，`load` / `show` /
  `go_to` / `compose` 只调模型、设 signal；原来的折叠测试移到 frontend。
- gui.md 的侧栏一节写明模型在哪。
