# 打开文件夹的同步插在完整同步的文件夹之间

- 日期：2026-10-21
- 类型：修复

## 背景

Issue #32。`sync_folder`（打开文件夹）、`replay_pending` 和后台同步的每一步都排在每账户的同步锁上，
而 `sync::run` 持锁贯穿整个账户的所有文件夹。首次同步一个大账户时，用户点开文件夹要等整个账户同步完
（几分钟），界面没有任何提示。

## 决策

- Issue 给了两个方案：`sync_folder` 用 `try_lock`、拿不到就返回 `Skipped::Busy` 让 GUI 显示"正在同步"；
  或完整同步在文件夹之间让出锁。选了后者：前者只是有了提示，用户点开的文件夹还是要等完整同步轮到它
  （可能排在几十个文件夹之后）。
- 让出的方式是完整同步不再整次持锁，而是对账 + 回放持一次、之后每个文件夹各拿一次。tokio 的 `Mutex`
  是公平的，等着的同步自然排在下一个文件夹之前，不需要等待者计数器，也不用给 `Permit` 加"让出再拿回"
  的接口（`Permit::run` 消耗 permit，工作 future 里拿不到它）。
- 插进来的同步做完的文件夹，完整同步不再同步一遍：没有 CONDSTORE 时那是一次全量 flags 拉取。判断用
  `last_synced_at` 是否晚于本次开始。它原先按秒记，同一秒里稍早（本次开始之前）的同步也会被当成"之后"，
  所以改为毫秒（项目未发布，直接改 `0001_init.sql` 的注释与 `set_synced`）。曾考虑在引擎内存里另记一份
  每个文件夹的 `Instant`，放弃：同一件事两个来源，且 `sync_one` 的新鲜度检查用的是数据库那份。
- 本次的开始时刻在第一次拿到 permit 之后才取（PR review 指出）：在等锁之前取的话，持锁中的
  `sync_folder` 可能在完整同步被调用之前就 EXAMINE 了，完成时间却晚于开始时刻，这个文件夹被跳过，
  其间到达的新邮件这次同步拿不到。
- 两次完整同步（手动 + 后台）现在按文件夹交替，而不是一个等另一个做完；各自跳过对方在自己开始后做完的，
  合起来大致只同步一遍。后台同步"是否该完整同步"的判断因此挪到拿锁之前。
- 只同步一个文件夹、只回放的路径仍整次持锁，它们本来就短。

## 方案

- `sync::run`：先在一次 permit 里 LIST、对账、回放、读文件夹列表；然后每个文件夹单独拿 permit，
  拿到后重新读它的 `FolderSyncState`（插进来的同步可能同步过它、开始了新 UIDVALIDITY，或对账删掉了它；
  过时的状态会让 UIDVALIDITY 比较出错），在本次开始后已完成同步的只报回放计数。`FolderSyncState` 带上
  `last_synced_at`，`sync_one` 与 `run` 共用 `synced_after` 判断。
- `sync::sync_account` 不再持 permit 连接服务器，改用从 `Permit::run` 提出来的 `Cancellation::run`
  监听账户删除（已删除返回 `AccountNotFound`，进行中返回 `Cancelled`）；`watch::work` 的完整同步在 permit 之外调用 `run`。
- 文档：`docs/architecture/sync.md` 新增"锁"一节，`architecture.md` 的每账户状态一句。
- 测试：`tests/sync.rs` 的 `sync_folder_goes_in_between_the_folders_of_a_full_sync`，用慢 `EXAMINE INBOX`
  把完整同步卡在第一个文件夹，期间同步最后一个文件夹，检查 EXAMINE 顺序为 INBOX、Later、Archive
  且完整同步没有再同步 Later。改动前该测试失败。`a_full_sync_doesnt_skip_a_folder_whose_sync_began_before_it`：
  `sync_folder` 已 EXAMINE、卡在 flags FETCH 时到达新邮件并调用完整同步，完整同步必须拿到它（开始时刻
  在等锁前取时失败）。全部测试、clippy、Dovecot 真实服务器测试通过。
