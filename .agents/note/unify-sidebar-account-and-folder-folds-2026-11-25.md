# 统一侧栏账户和文件夹的折叠

- 日期：2026-11-25
- 类型：修复（#378）

## 背景

侧栏里点账户标题整行就折叠、展开，文件夹的子文件夹却只能点行尾的折叠按钮；两个箭头也不一样：账户的是
12 pt、淡色的图标，直接放在行尾，文件夹的是 24 宽的按钮（15 pt 图标），两者不在同一列。

## 决策

- 账户标题的箭头换成和文件夹相同的 `fold_button`（`Size::Row`，`mr(-4)`），同一列、同样的样子，并有读屏用的
  名字（Collapse / Expand 加地址）。整行按下仍然折叠：标题没有别的事可做。
- 文件夹行按下要选中文件夹，不能再拿来折叠。和用户确认后采用双击：单击选中，双击（`PressEvent::clicks == 2`）
  选中并折叠或展开，与 Thunderbird、Outlook 一样。考虑过的其他做法：单击选中并展开（收起仍要点按钮，还是不一致）；
  账户也只认箭头（标题整行变成点了没反应）。
- 账户标题双击会折叠两次（展开又收起），与原来一样，不特别处理。

## 方案

- `crates/catmail-gui/src/mailbox/sidebar.rs`：`heading` 用 `fold_button`；`place_row` 的 `on_press` 看 `clicks`。
- GUI 测试 `accounts_and_folders_fold_and_unfold`：双击 Projects 收起其子文件夹；账户的折叠按钮收起、展开账户。
- `docs/architecture/gui.md` 的"折叠"补上这两条。
