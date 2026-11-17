# 全仓库审查：修复确认的问题，删掉不用的部分

- 日期：2026-11-17
- 类型：修复

## 背景

以仓库 owner 的角度把十个 crate、文档、CI 和打包脚本通读了一遍，按 crate 分头审查，再逐条核实。
这里只记确认后改掉的；拿不准的、涉及取舍的留给了讨论，没有动。

## 决策

- 只改能用测试或编译证实的。审查里"`Listing::keys` 没有调用方"是错的（GUI 以 `Listing::keys` 方法引用的
  形式在用），删掉后编译失败，已恢复：删除"无调用方"的东西一律以全工作区编译为准。
- 大块的结构调整（`client.rs`、`drafts.rs`、`store/ops.rs` 的拆分，写信窗口的入口移到 `mailbox/`）没有做：
  是纯搬运，但改动面大，先商量。
- 列表刷新改为多读一页再截到原来的最后一行，而不是按"上次的行数"读：后者在来新邮件时把末尾的行挤出去。
  代价是每次刷新多读至多一页。

## 方案

修复（各带测试，除非注明）：

- **core**：Gmail 上同一标签的两次改动会折叠成一条；位置文件夹 SELECT 被拒时，被折叠的那条永远不算"看过"，
  `ops::replay` 无限重试并一直占着账户的同步许可。`seen` 现在包含 `ReplayOp::settled()`
  （`tests/gmail.rs::a_refused_location_ends_the_replay_of_folded_changes`，去掉修复会超时）。
- **store / core**：`Messages::delete_gone` 不记显示这封信的标签文件夹，Gmail 上打开一封已被别处删除的邮件后，
  标签列表里的那一行留着不走。现在记下并由 `body::forget` 一并发事件。
- **store**：`note_shown_in` 给了 UID 时按 `(folder_id, uid)` 查，而不是读文件夹的每一行；
  `Accounts::delete` 先删草稿，不再依赖建表顺序决定的级联先后（草稿的服务器副本有 CHECK）；
  去掉没有查询用的 `messages_message_id_hdr` 索引，给 `drafts.reply_to_message_id` 的外键加索引；
  `BlobStore::root` 删除，`BlobStore::remove`、`Messages::delete_uids` 不再公开。
- **proto**：`without_addresses` 对一行 `<` 是平方级的（摘要对每封正文都跑），改为每个字节只读一次；
  骨架的 boundary 一遍算出；IMAP 写命令的超时改为每 64 KiB 一次，大附件的 APPEND 在慢线路上不再必然超时；
  IDLE 发出 DONE 后服务器再发 `+` 不再回到 idling（会再次轮询已结束的 `stop`，panic）。
- **html**：加了作用域的样式表里，`body ~ x`、`html + x` 这类兄弟选择器改写后会匹配到引文之外，现在整条规则丢弃；
  纯文本引用的 `<blockquote>` 嵌套最多 32 层；`catmail-html` 补上 `#![forbid(unsafe_code)]`。
- **frontend**：列表刷新不再把末尾的行（和选中的那封）挤出去；`tel:5551234` 不再被当成"主机:端口"。
- **gui**：搜索时点侧栏里已选中的位置，回到该位置而不是没有反应（没有驱动窗口的测试，未能运行验证）。
- **cli**：`watch` 和草稿上传的警告里，服务器给的错误文本先转义再打到终端；`compose --send` 没有收件人时
  先报错，不留下草稿。

删除：24 个没有被引用的 Heroicons SVG；`catmail-proto` 的 `url`、`catmail-gui` 的 `jiff` 依赖和重复的
`NSGraphics` feature；`sanitize.rs` 里匹配不到的 `action`。

文档与注释：README、`cli.md`、`packaging.md`、`testing.md`、`protocol.md`、`sending.md`、`storage.md`、
`gui.md`、`background.md`、`mcp.md` 里与代码不符的几处，以及 GPUI 时期留下的几句注释。

验证：`cargo fmt`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`
（本机，macOS；没有跑 Linux / Windows 和 Dovecot 的测试）。
