# 修复 M5 PR 评审的四个问题

- 日期：2026-10-20
- 类型：修复

## 背景

M5 GUI PR 的评审指出四个问题，逐一核实都成立：

1. **Composer 结束过程中仍可编辑。** 发送 / 关闭 / 丢弃开始后只禁用了按钮，地址、主题和正文仍可输入；
   `run_finish` 已取走内容，`changed()` 又忽略此后的改动，窗口成功关闭后这些输入悄悄丢失。拖入大附件
   后立刻发送（发送要等附件存完）就能复现。
2. **同步期间到期的草稿要等到下次完整同步。** 后台一步的上传阶段跳过刚改过的草稿，若这一步余下的
   同步超过了它剩余的等待期，`Engine::held_drafts` 在算下次唤醒时会顺手删掉已到期的记录，`due`
   退回完整同步的间隔（默认 15 分钟）。只有第一次修改会唤醒后台任务，没有其他动静时草稿就一直不上传。
3. **"移动到…" 列表分不清同名文件夹。** 列表是平的，只显示树上一层的 label：`Clients/Invoices` 和
   `Personal/Invoices` 都是 `Invoices`，按父级名字也搜不到。
4. **JSONC 尾逗号后跟注释时没被去掉。** 判断尾逗号只跳过空白，`{"name": "x", // c\n}` 的逗号保留，
   注释删掉后剩下非法 JSON，Windows Terminal 配色导入失败。

## 决策

1. 用 `readonly` 而不是 `disabled`：外观不变、仍可选中复制，只是拒绝修改；`disabled` 会让每次关窗都
   闪一下灰色。失败时 `finishing` 清空，字段自动恢复可编辑。
2. 到期记录只在后台一步**开始时**清掉（`forget_settled_drafts`），这一步一定会把它们当作已稳定上传；
   算唤醒时间（`next_draft_due`）不再删记录，已到期的返回 0，下一步立即开始。放弃在
   `draft_sync::upload` 里清：它在没有待上传草稿或没有 Drafts 文件夹时提前返回，记录永远清不掉，
   `due` 恒为 0 会空转。
3. 显示和搜索都用文件夹在服务器上的完整路径（按 `folder.name` 和分隔符拆开，`Clients / Invoices`），
   侧栏为某个角色命名的那个文件夹仍用 "Sent" 等名字。没有用侧栏树的 label 拼路径：侧栏把 Drafts 的
   子文件夹提到了顶层，`Drafts/Templates` 会和顶层的 `Templates` 同名；同一角色的第二个文件夹也会和
   第一个同名。
4. 分两遍：先去注释，再在结果上去尾逗号。顺带去掉了每个逗号都复制一次剩余文本的 O(n²)。

## 方案

- `crates/catmail-gui/src/composer/view.rs`：头部输入框和正文在 `finishing` 时 `readonly`。
- `crates/catmail-core/src/engine.rs`：`held_drafts` 拆成 `held_drafts`（只读）、`next_draft_due`、
  `forget_settled_drafts`；`watch.rs` 在一步开始时调用后者，`due` 为 0 时跳过等待直接下一步。
  `docs/architecture/sending.md` 同步更新。
- `crates/catmail-testkit/src/imap.rs`：新故障 `slow`，让以某段文本开头的命令延迟应答。
- `crates/catmail-gui/src/mailbox/folders.rs`：`AccountFolders::paths`；`commands.rs` 的移动列表用它。
- `crates/catmail-gui/src/theme/import/structured.rs`：`drop_comments` + `drop_trailing_commas`。

测试（去掉修复后都会失败）：`nothing_can_be_typed_while_the_message_goes`、
`a_draft_that_settles_during_a_sync_is_uploaded_after_it`（EXAMINE 延迟到超过等待期）、
`paths_tell_folders_of_the_same_name_apart`、`json_trailing_commas_go_also_before_a_comment`。
