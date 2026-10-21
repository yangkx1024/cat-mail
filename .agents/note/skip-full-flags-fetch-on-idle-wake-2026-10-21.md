# 无 CONDSTORE 时 IDLE 唤醒不再全量拉 INBOX flags

- 日期：2026-10-21
- 类型：修复

## 背景

Issue #31。没有 CONDSTORE 的服务器上，`sync_folder` 每次都发 `UID FETCH 1:<max> (UID FLAGS)`。
后台同步每次 IDLE 报告变化后都同步 INBOX，于是 5 万封的收件箱每来一封新邮件就是约 1.5 MB 的严格
命令应答（整个缓冲在内存里）和一个 5 万行的大事务。国内不少服务商没有 CONDSTORE。

## 决策

- 不是简单地让 IDLE 步骤一律跳过 flags：那样别的客户端（手机上读了邮件）改的 flags 要等 15 分钟的
  完整同步。连接层另记"IDLE 期间收到 FETCH（或无法解析的行）"为 flags 变化
  （`Session::flags_changed`），后台同步只在这时拉全部 flags；新邮件、删除、本地改动唤醒和轮询都不拉。
  这个信号保留到 INBOX 真的同步成功为止（PR review 指出：INBOX 的非致命错误被转成 `Wait::Poll`，
  原先会清掉信号，flags 一直过期到下一次完整同步）。完整同步那一步不特殊处理：若它的 INBOX 失败、
  随后的 `sync_inbox` 成功却没拉 flags，信号就丢了；代价只是两者恰好同时发生时多拉一次。
- 有 CONDSTORE 时照旧总是带 `CHANGEDSINCE` 拉：跳过的话写入的 HIGHESTMODSEQ 游标会声称变化已看过。
- 跳过 flags 的同步照样写 UIDNEXT 游标，但不更新 `last_synced_at`：否则用户打开 INBOX 时
  `sync_folder(max_age)` 会以为它刚同步过，轮询模式下就永远拿不到别人改的 flags。
- 全量 flags 按本地邮件数分窗口（每窗口 5000 封），而不是按 UID 值分：UID 稀疏的文件夹按 UID 分
  会发出大量空命令。窗口由一次读出的本地 UID 列表 `chunks` 而来（5 万封约 200 KB，远小于要限制的
  应答），比每个窗口查一次边界简单。有 CHANGEDSINCE 时仍是一条命令，答复本来就小。

## 方案

- `catmail-proto`：`Connection` 增加 `flags_changed`，`Session::flags_changed()`；`fetch_flags`
  改为接受 UID 范围。
- `catmail-store`：`Folders::set_synced` 增加 `flags_refreshed`。
- `catmail-core`：`sync_folder` / `sync_inbox` 增加 `refresh_flags`，flags 分窗口、每窗口一个事务；
  `watch.rs` 记下 IDLE 报告的 flags 变化，传给下一步，直到 INBOX 同步成功（新增 `Wait::InboxFailed`）。
- `catmail-testkit`：`uid_set_contains` 不再对每封邮件重算最大 UID（大邮箱上 FETCH 是平方复杂度）。
- 测试：1.2 万封、UID 稀疏的无 CONDSTORE 收件箱，第二次同步的 flags 分两个窗口并正确应用；后台
  同步在无 CONDSTORE 服务器上新邮件不拉 flags、别的客户端改 flags 时拉一次（去掉修复会失败）；
  flags 应答被注入一次无法解析的行时下一步重试（`a_failed_flags_refresh_is_tried_again`）；
  proto 的 IDLE flags 变化、UID 范围测试；store 的 `set_synced` 测试。
- 文档：`docs/architecture/sync.md`、`protocol.md`。
