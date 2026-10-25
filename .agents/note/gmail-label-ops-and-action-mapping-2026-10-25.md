# Gmail 的标签操作与动作映射

- 日期：2026-10-25
- 类型：功能

## 背景

[位置与标签](../../docs/architecture/labels.md)迁移步骤 5（#128）。#127 之后 Gmail 一封邮件一行，但操作
队列还按"每个邮箱是文件夹"做：在 INBOX 里把邮件移到 Work 会把行搬进 Work 这个标签文件夹，归档因为
Gmail 没有归档文件夹而不能用；草稿旧副本的删除按本地文件夹的 UIDVALIDITY 核对，草稿邮箱是标签、没有
这个值，删除一直待回放；Gmail 自己把发出的邮件存进已发送，我们再 APPEND 一份；回放之后还按 Message-ID
找"副本"（#121 的过渡机制，#120）。

## 决策

- **规则在 types 的 `MovePlan`**：移动、归档做的是"换位置 / 加标签 / 去标签"的组合，store 和 GUI 用同一个
  函数（GUI 据此决定归档按钮和"移到"的目标），取代 `archive_folder`。Gmail 上的"当前标签"只有前端知道，
  所以 `move_messages` 多一个 `shown_in`（搜索结果为空）；CLI 的 `mv` 对应 `--from`。考虑过从邮件推断，
  但一封邮件可以有多个标签，推不出来。
- **标签 op 每个标签一条**（`kind = 'label'`，`target_folder_id` 是标签，加一列 `label_add`），而不是一条
  op 带一组标签：复用 `target_folder_id` 的复合外键（标签删除时 op 随之删除），不用子表；分轮、按标签
  分组回放，撤销只逆推同一个标签的操作，比 flags 的位运算还简单。
- **"移出已删除到 INBOX"先 MOVE 再加标签**：设计要求如此，在已删除里给邮件加 `\Inbox` 的效果没验证过。
  做法是一条通用规则：在一次移动之后做的标签改动等这次移动（`pending` 不给出它，`WAITING`），随
  `moved()` 改指新位置；回放按（文件夹, op）记看过的，对还有没看过的 op 的文件夹再走一遍，所以同一次回放
  里做完。原来按文件夹记"走过"，跟到副本文件夹的 op 若那个文件夹已走过要等下次，也一并改好了。移动被
  拒绝时等它的标签改动一起失败（它们是冲着新位置去的）。
- **移到已删除 / 垃圾邮件时 Gmail 去掉的标签，本地立即去掉并入队**（`MovePlan.remove`），等移动做成后在
  新位置回放（服务器上已不改变什么，多一条 STORE）。最初是移动做成后才在本地去掉（`drop_labels_left`），
  移动被拒绝时不用恢复；codex 评审指出两处问题：移到已删除正在途中时又移回 INBOX，本地 INBOX 还在、
  `+\Inbox` 不入队，服务器上 INBOX 丢了；移出已删除到 INBOX、回放前又删除，等移动的 `+\Inbox` 被放出来
  在已删除里发出（效果没验证过）。改为本地事先就知道服务器会做的改动，第一处随之解决；第二处靠"同一个
  标签的改动一次回放只发最后一条"（`supersede`，被替代的随它了结，成功、拒绝一起）。同一次评审还指出
  同步刷新 flags 时用服务器的标签覆盖了还没回放的标签改动（归档后同步，INBOX 又出现），改为像 flags 一样
  叠加（`Messages::set_labels`）。
- **回放按 op 自己的 UIDVALIDITY 核对**，不一致的立即标 `conflict`（`end_generation`，与同步发现新代际时相同），对所有文件夹一样。原来
  位置文件夹是"跳过、等同步发现新代际再标"，结果相同，只是早一步；一套规则比按文件夹种类分两种简单。
  `ReplayFolder.uidvalidity` 随之删除。
- **Gmail 不 APPEND 到已发送**只在"同一个登录、SMTP 与 IMAP 同一个域"时：身份可以经别的 SMTP 服务器发信，
  Gmail 看不到那些，跳过就丢了已发送里的副本。考虑过写死 `smtp.gmail.com`，但假服务器测不到。
- 删掉 `PendingOps::label_copies`、`Engine::add_label_copies` 等、`sync::drop_vanished`、`LABEL_ACCOUNTS`
  和它们的测试，以及只剩它们在用的假服务器故障 `drop_on_open`；`uid_search_uids`（打开已消失的邮件时仍用）保留。
- 没做：GUI 里列表行显示标签、侧栏分开位置和标签（#129）；归档按钮在"所有邮件"里不在 INBOX 的邮件上也
  可用（什么也不做），要等列表摘要带上标签才能更准。

## 方案

- types：`MovePlan::of`（不能移动时返回原因 `Unmovable`）/ `archive` / `can_archive`、`Folder::is_label`、
  `GmailLabel::for_mailbox`（系统标签的角色只列一处，proto 的线上写法有测试对齐）、`OpKind::Label`。
- store（`ops.rs`）：`move_messages(shown_in)`、`archive_messages`、`label_messages`、`carry_out` /
  `label_one` / `set_label`；`pending` 的顺序（flags、标签、移动删除）与 `WAITING`；`ReplayFolder.ops`（回放据此判断哪些文件夹要再走一遍）；
  `failed` 拆出 `fail`、`fail_waiting`、`unwind_label`；`supersede`；`end_generation` 公开给回放用；
  `0001_init.sql` 加 `pending_ops.label_add` 和注释。
- core：`ops.rs` 回放标签批次、按 op 核对代际、再走一遍；`MailClient::move_messages(shown_in)`、
  `label_messages`；`send.rs` 的 `saves_sent_mail`；删掉过渡机制。
- CLI：`mv --from`、`catmail label --add/--remove`，`ops` 的列表与 JSON 带上标签改动。GUI：移动带上正在看的
  文件夹，归档与"移到"的目标按 `MovePlan`。
- 文档：`labels.md`、`op-queue.md`、`sending.md`、`sync.md`、`gui.md`、`cli.md`、`architecture.md`、AGENTS.md。
- 验证：types 的 `MovePlan` 单元测试；store 测试（标签间移动、归档、撤销、等移动的标签改动与一起失败、
  移到已删除 / 垃圾邮件去掉的标签；评审的三处各一个测试，修复前都失败：`sync_keeps_unreplayed_label_changes`、
  `moved_back_while_the_move_to_trash_is_made_it_gets_inbox_again`、
  `a_restore_cancelled_before_it_is_made_adds_no_inbox_in_trash`）；假服务器 `tests/gmail.rs`（标签间移动只 STORE X-GM-LABELS、归档、
  删除再移回 INBOX 同一行带正文且一次回放做完、拒绝后撤销、草稿旧副本删除及另一代的成为冲突、发信不
  APPEND、回放前取消的恢复不在已删除里加 `\Inbox`）；`a_rebuilt_folder_turns_changes_into_conflicts` 改为回放时就成为冲突。`cargo test --workspace`、
  clippy、Dovecot 与 submission 真实服务器测试通过。CI 里没有真的 Gmail。
