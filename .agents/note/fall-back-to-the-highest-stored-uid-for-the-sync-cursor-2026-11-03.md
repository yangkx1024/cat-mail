# EXAMINE 不给 UIDNEXT 时游标退回本地最大 UID + 1

- 日期：2026-11-03
- 类型：修复

## 背景

issue #206。网易邮箱（Coremail）的 EXAMINE 不给 UIDNEXT，同步完成时 `set_synced` 记下的游标一直是
NULL：`Fetch::arrived_from` 为 `None`，网易账户从不发新邮件通知；`new_and_vanished` 没有游标，每次
同步都 `UID SEARCH ALL` 并载入本地全部 UID。

## 决策

- 游标退回本地最大 UID + 1。同步完成说明这之下的邮件都已存下，UID 只增不减，之后到的邮件一定在它
  之上，满足游标"之下都已存下"的约定。
- 不用 `UID SEARCH UID *` 取服务器的最大 UID：同步之后才到、还没存下的邮件会落到游标之下，被漏掉。
- issue 里"缺 UIDNEXT 时快速路径只比条数"不安全：删一封又到一封时条数不变，新邮件会被漏掉。快速
  路径保持原样（没有 UIDNEXT 时不成立），落到 `UID SEARCH UID <游标>:*` 那条路，条数对得上就不做
  全量比对。
- 退回放在 `Folders::set_synced` 的 SQL 里（`ifnull(?2, max(uid) + 1)`），而不是在 `sync.rs` 用同步前
  读到的 `max_uid`：取的是本次同步存完之后的值、和游标在同一条语句里，`sync_folder` 和 `arrivals`
  两处写游标都自动覆盖（`arrivals` 原本会用 NULL 覆盖游标）。
- 已知边界：游标可能比服务器的小（最高的被删除，或正在本地移走被 `upsert` 跳过）；本地移走后操作被
  丢弃、邮件被重新拉回时会被当成新邮件通知一次。很少见，只写进注释和文档。
- 退回值封顶在 `u32::MAX`：本地最大 UID 已是上限时，加一后的 4294967296 读回 `Option<u32>` 会报
  `IntegralValueOutOfRange`，`sync_state` 读不出来，之后每次同步都在 EXAMINE 之前失败，连 UIDVALIDITY
  变化都发现不了。封顶后游标仍满足"之下都已存下"，与 `copy_boundary` 的 `saturating_add` 一致。
- 复制检查点仍然不能用这个游标（#203）。

## 方案

- `catmail-store/src/folders.rs`：`set_synced`（参数改名 `server_uidnext`）在它为 `None` 时取本地最大 UID + 1；单元测试
  `without_the_servers_uidnext_the_cursor_is_past_the_highest_stored_uid`。
- "本地最大 UID"的 SQL 片段由 `max_stored_uid!` 宏提供，`SYNC_STATE` 和 `set_synced` 共用，免得两处
  规则（比如以后排除某些行）改了一处忘了另一处。顺带把该文件测试里重复五次的"建账户、对账一个文件夹、
  取 id"抽成 `one_folder`。
- `catmail-testkit`：`bare_status` 拆成 `bare_status`（STATUS 回空列表）和 `no_uidnext`（EXAMINE/SELECT
  不给 UIDNEXT）；`tests/ops.rs` 的网易场景两个都开。
- `catmail-core/src/sync.rs`：只改注释。
- 测试：`tests/sync.rs` 的 `new_mail_alone_does_not_list_every_uid` 改为有无 UIDNEXT 各跑一遍，并检查
  `NewMail`：新邮件只发 `UID SEARCH UID 4:*` 且被通知；删一封又到一封时两边都不漏；最高一封被删、
  游标落后于服务器 UIDNEXT 后，新邮件仍被通知。没有修复时无 UIDNEXT 那一遍失败（不发 `NewMail`）。
- 文档：`docs/architecture/sync.md` 的同步步骤和"游标与断点续传"。
