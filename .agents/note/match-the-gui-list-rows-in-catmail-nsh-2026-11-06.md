# nsh 版列表行的悬停、选中和间距对齐 gpui 版

- 日期：2026-11-06
- 类型：功能

## 背景

nsh 版的列表行没有指针悬停时的背景，选中色也和 gpui 版不一样，行贴着列表两边、上下没有间隔。gpui 版用的是 GPUI
Kit 的 list：两边 `px_3`，每行 `my_0p5`，选中用 `list_active`（半透明的主色），悬停用 `list_hover`。

## 决策

- 颜色取主题里的 `list.active.background`、`list.hover.background`。catmail-frontend 推导主题时已经会填这两个；
  没有时退回 accent。
- 悬停的行记在列表级的 `Hovered` 里，不在每行各记一个：虚拟列表的格子滚到别的行时，指针不会跟着走。消息行原来
  就用它决定要不要显示悬停时的按钮，现在背景也读它，同一件事只有一处在记。
- 两边的边距用虚拟列表自己的 `content_padding_x`，不再给每个格子多包一层 view。

## 方案

`crates/catmail-nsh/src/list.rs`（`row_box`：圆角、上下各 2、背景；`Hovered` 改为按 `RowId` 记，草稿和发件箱的行
也用它）、`crates/catmail-nsh/src/theme.rs`（`list_active`、`list_hover`）。验证：和 gpui 版截图对比；catmail-nsh
的 clippy `-D warnings` 和测试通过。
