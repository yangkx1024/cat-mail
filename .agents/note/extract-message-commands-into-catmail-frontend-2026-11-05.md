# 把邮件命令的模型抽进 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

nsh 分支的 P4 要做读信区的命令：归档、删除、移动、标签、已读、旗标。gpui 版里这些命令的规则都在
`mailbox/commands.rs`、`message_commands.rs`、`message_list` 和 `reader/message.rs`，跟 GPUI 的类型写在一起。
按"纯逻辑共用，不复制"，先把与 UI 库无关的部分抽出来。

## 决策

- `catmail_frontend::commands`：`MessageCommand` 和它的名字（开关按邮件共有的标记说做什么）、`Applies`
  （字段改成私有，用 `flags()`、`enables(command)` 读）、`common_flags`、`one_account`、`toggled`、`what`、
  `archivable`、`movable`、`move_targets`、`label_change`、`deletion`（`Deletion`：要删的、`DeleteMode`、
  永久删除时的确认文案）、`refused_change`，以及文件夹选择框的过滤和空列表文案。图标留在各前端。
- `catmail_frontend::list::Removal`：移走后打开下一封的等待（`removing`、编号、`settled`）。读取的计数留在
  列表里，因为 gpui 版的 `reveal` 也用它；`Removal` 只接收读取的编号。
- `catmail_frontend::reader`：`Header`（标题、发件人、收件人一行、Details 的完整头部）、附件列表和附件按钮的
  文字、保存对话框的起始目录。
- `catmail_html::data_url`（经 `catmail_core::html` 导出）：`embed_inline_images` 里构造 `data:` URL 的代码公开出来，
  nsh 版把远程图片填进页面时用同一条规则（MIME 类型只留合法字符）。
- gpui 版行为不变，只是改为调用这些函数。

## 方案

- 新文件 `crates/catmail-frontend/src/commands.rs`、`reader.rs`；`list.rs` 加 `Removal`。
- gpui 版：`message_commands.rs` 只剩图标和按钮，`commands.rs` 的归档、移动、删除、标签、被拒改动改用共用函数，
  `message_list` 的三个字段换成 `Removal`，`reader/message.rs` 从 `Header` 取文字，`folder_picker.rs` 用共用的过滤。
- 测试：frontend 新增开关的名字、选择框过滤、`Removal` 的等待（先于改动的读取不算、仍在列表里就不再等、
  先归档再删除只认最后一次、失败立即不等）；gpui 的 157 个测试照常通过（读信、标签、勾选的集成测试覆盖了
  这些命令和移走后打开下一封）。
