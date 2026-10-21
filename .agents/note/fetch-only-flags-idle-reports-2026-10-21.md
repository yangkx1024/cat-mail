# 无 CONDSTORE 时只拉 IDLE 报告了 flags 变化的邮件

- 日期：2026-10-21
- 类型：功能

## 背景

#31（PR #51）之后，无 CONDSTORE 的服务器上，IDLE 报告 flags 变化时后台同步仍要按窗口拉 INBOX 的全部
flags。别的客户端改 flags（手机上读了一封邮件）和新邮件差不多一样频繁，五万封的收件箱每次仍要十个窗口、
十个事务。IDLE 期间的 `* n FETCH (FLAGS …)` 本来就指明了是哪封。

## 决策

- 序号只在下一个 EXPUNGE 之前有效，而回放、重新 EXAMINE 都可能带来 EXPUNGE。所以在 proto 的
  `Session::idle` 里、IDLE 一结束就用 `FETCH <序号> (UID)` 换成 UID：RFC 3501 §7.4.1 不许服务器在按序号的
  FETCH 期间发 EXPUNGE，换出来的一定对。序号的生命周期留在 proto，core 只看到 UID。有 CONDSTORE 时不换
  （同步按 MODSEQ 找变化），省掉多数服务器上的这次往返。
- `Session::changed_flags` 只是最近一次 IDLE 的报告：IDLE 一有变化就返回，打开文件夹之前不会再 IDLE，
  所以只在 `watch.rs` 一处累积，不设上限（`sync_folder` 本来就按窗口分批，上限只会把两条 FETCH 变成十个窗口）。
- 只换 UID，不顺带拿 FLAGS 直接写库：之后的回放会改服务器上的 flags，回放完成后 `pending_ops` 不再叠加，
  旧的 FLAGS 会把本地状态改回去。flags 由随后的 INBOX 同步按 UID 拉，并沿用 `sync_folder` 的
  UIDVALIDITY 检查。
- IDLE 期间收到的 EXPUNGE 让已记的序号前移，而不是退回全量；VANISHED 按 UID 删除、对不上序号，已记有
  序号时退回 `Unknown`（不启用 QRESYNC，实际不会出现）。超过一千封不再逐个记，也是 `Unknown`。
- 复用 proto 的 `ChangedFlags`（`Uids` / `Unknown`）表示"要刷新哪些 flags"：完整同步就是"不知道哪些变了"，
  即 `Unknown`，不另造一个 core 类型。
- 考虑过断线重连后一律全量拉（断线期间的变化听不到），没有做：总被代理断开 IDLE 的连接会退回 #31 的
  开销；与轮询一样，交给完整同步和打开 INBOX 时的同步。

## 方案

- `catmail-proto`：`Connection` 记 IDLE 期间 FETCH 的序号（有序 `Vec`，`take_changed_flags`，EXPUNGE 时
  原地前移）；`Session::changed_flags()` / `ChangedFlags`，IDLE 结束时换成 UID；`fetch_flags` 改为接受若干
  UID 范围，合并相邻范围（与 `uid_set` 共用 `merge_ranges`），应答按二分查找过滤。
- `catmail-core`：`sync_inbox` / `sync_folder` 接受 `&ChangedFlags`；`watch.rs` 累积并传给下一步。
- `catmail-testkit`：支持按序号的 `FETCH <set> (UID)`（按会话已知的视图编号）；UID 集合与序号集合共用
  `set_contains`。
- 测试：连接层脚本服务器验证 FETCH 之后的 EXPUNGE 让序号前移、被删的那封去掉；假服务器上 EXPUNGE 之后报告
  的 FETCH 换出正确的 UID、无法解析的行得到 `Unknown`；后台同步只发 `UID FETCH 2 (UID FLAGS)`。
- 顺带修正 `sync.md` 里 #31 插错位置、把"重新 IDLE"一条的 NAT keepalive 那句分到"INBOX 的 flags"下的问题。
