# nsh 版邮件列表

- 日期：2026-11-05
- 类型：功能

## 背景

catmail-nsh 对齐 gpui 版的 P3（`crates/catmail-nsh/ROADMAP.md`）。原来的列表只读文件夹和 All Inboxes 的
邮件，每行三行字（发件人、主题、摘要），没有分组、头像、勾选、搜索，草稿和发件箱是空的；刷新时整个替换。

## 决策

- 模型用 `catmail_frontend::list`（见 `extract-list-rows-into-catmail-frontend-2026-11-05.md`），nsh 版只写界面
  （`src/list.rs`）。
- **分组标题是列表里的行**：nsh `virtual_list` 的 `sticky_header_indices` 只能在建表时给一次，而分组随数据变。
  gpui 版的 section 标题也不吸顶，所以不算差别。列表把标题和行拍平成 `Item`，`keys` 用行的身份。
- **格子会被回收**：一个格子会轮流显示标题、邮件、草稿、发件箱，所以按位置算出种类（`computed`），种类变了
  才用 `dynamic` 换内容；字段的绑定照常跟着位置。
- **选中按身份**（`Selected(Option<RowId>)`），不按下标；重读时用 `list::reselect` 判断它还在不在，不在就清空
  读信区。没有用户移走邮件这回事（P4），所以 `removing` 总是 `None`。
- **读信区只在有选中时出现**，列表这时宽 420 pt，否则占满窗口，对齐 gpui 版。两种行形在 `reading()` 变化时
  整行换；切换时把选中（或刚关上）的那行滚到中间。
- nsh 列表按行量高度，两行时主题按实际行数占高，不必像 gpui 版那样总占两行。
- 头像的色相用固定的六个，按明暗调透明度；旗标用琥珀色：nsh 的主题没有这些颜色，P7 主题对齐时再接上。
- 勾选菜单是原生 `popup_menu`。Escape 关上读信区。
- 悬停的命令按钮、有勾选时的批量工具栏要等 P4 的命令；草稿、发件箱的右栏随 P5、P6，现在选中它们右栏是空的。

## 方案

- 新增 `src/list.rs`；`src/app.rs` 去掉原来的列表，选侧栏时 `list::show`，打开的邮件跟着 `Selected`；
  `src/sidebar.rs` 读完账户后更新标签名（`list::set_labels`），选中时不再自己清读信区；`src/icons.rs` 加对钩、
  放大镜、回形针、实心旗标。
- `CATMAIL_NSH_OPEN` 改由列表在第一次读完后打开那一行。
- 验证：nsh 版 clippy 通过。真实数据目录截图：All Inboxes 按 Today、Yesterday、"Tuesday, November 3"
  分组，一行排开时头像、发件人一栏、主题、附件图标、时间；打开一封后变两行，选中的行有底色。
  用户手动试过（这台机器没有模拟输入的工具）：打开与 Escape、切换行形时的滚动、点头像和 X 勾选、勾选框与菜单、
  搜索（本地结果后再加服务器的）、选侧栏清空搜索、Gmail 标签、悬停显示收件人、Drafts 和 Outbox 的列表。
