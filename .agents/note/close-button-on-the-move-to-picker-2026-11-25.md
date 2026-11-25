# "Move to"、"Labels"对话框加关闭按钮

- 日期：2026-11-25
- 类型：修复

## 背景

"Move to"和"Labels"对话框（`mailbox/picker.rs`）没有按钮可关，压暗的背景点了也没反应（nsh 的 `Modal` 只替平台
自己画的背景处理按下，应用画的归应用），只能按 Escape。

## 决策

- 在标题行右边放一个关闭按钮（`icon_button`，XMark，`Size::Row`，向上下伸出 4，同设置窗口的做法），按下与
  Escape 一样关掉。
- 没做"点背景关掉"：用户认为加一个关闭按钮就够了。`form::modal` 的文档原来说"旁边按一下"也会取消，实际上
  没接，改正了文档。

## 方案

- `crates/catmail-gui/src/mailbox/picker.rs`：标题行加关闭按钮。
- `crates/catmail-gui/src/form.rs`：`modal` 的文档去掉"旁边按一下"。
- GUI 测试 `the_move_to_picker_closes_from_its_button`。
- `docs/architecture/gui.md`："Move to"、"Labels"对话框一条补上。
