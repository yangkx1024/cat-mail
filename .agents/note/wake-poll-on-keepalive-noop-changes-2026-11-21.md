# 用 keepalive NOOP 的答复提前发现 INBOX 变化

- 日期：2026-11-21
- 类型：功能

## 背景

#227 为不支持 IDLE 的后台同步轮询加入了每 `SyncSchedule::keepalive`（45 秒）一次的 `NOOP` 保活，但当时只用来判断连接是否存活，答复里携带的邮箱变化被直接丢弃，新邮件仍要等到 `SyncSchedule::poll`（2 分钟）到期才会同步（#231）。

用真实网易 163 账户（Coremail）双连接实测确认：
- 新邮件到达（`APPEND INBOX`）约 5 秒刷新窗口后，`NOOP` 返回 `* n EXISTS`；
- 删除邮件（`EXPUNGE`）约 5 秒后，`NOOP` 返回 `* seq EXPUNGE` 与 `* n EXISTS`；
- 仅修改 flags 时，网易不发 `FETCH FLAGS`，只重发相同数量的 `* n EXISTS`（不会误触发 `exists != n`）。

因此该优化在网易 163 上能将新邮件延迟上限从约 2 分钟缩短到约 45 秒。

## 决策

- **区分 `Session::noop()` 与 `Session::poll()`**：连接池保活、退避与失败重试等只需保活的场景继续用 `Session::noop()`，不看答复里的变化，避免在无 CONDSTORE 服务器上因为别的客户端改了 flags 而多发一次无人使用的 `FETCH <seq> (UID)`。轮询 INBOX 用新增的 `Session::poll()`（`IDLE` 的非 IDLE 对等方法），追踪 NOOP 中的 `FETCH` 与畸形行并沿用原有的 `resolve_changed_flags` 逻辑。
- **轮询路径复用 `busy` / `BUSY` 防抖**：连续超过 `BUSY`（10）次由 NOOP 提前唤醒（例如服务器每次 NOOP 都返回无法解析的行或持续报变化）后不再提前结束等待，退回按完整 `poll` 间隔检查，直到收到一次安静的 NOOP 才恢复提前唤醒。
- **只在正常轮询 INBOX 时使用 `poll`**：通过 `Wait::watches_inbox()` 决定 `poll_inbox`，退避（`busy > BUSY`）、INBOX 同步被拒（`Wait::InboxFailed`）及无 INBOX（`Wait::NoInbox`）均只走 `pool::ping` 保活。

## 方案

- `crates/catmail-proto/src/imap/connection.rs`：新增 `Connection::noop()`，与 `IDLE` 共用变化追踪模式（`Mode::Noop`），把 NOOP 中的 `FETCH` 和畸形行计入 `changed` / `changed_flags`。
- `crates/catmail-proto/src/imap/session.rs`：保留 `Session::noop()` 供纯保活使用，新增 `Session::poll() -> Result<bool>` 返回是否有变化并调用 `resolve_changed_flags()`。
- `crates/catmail-core/src/pool.rs`：`Pinged::Alive` 携带 `changed: bool`，拆出 `pool::ping`（纯保活）与 `pool::poll`（轮询变化）。
- `crates/catmail-core/src/watch.rs`：区分 `Wait::Poll` 与 `Wait::NoInbox` 并提供 `Wait::watches_inbox()`；`KeepAlive` 拆为 `Alive { changed }`、`Closed` 与 `OverLimit`；轮询路径对连续 NOOP 提前唤醒套用 `busy` / `BUSY` 保护。
- `crates/catmail-testkit/src/imap/mod.rs`：假服务器的 `NOOP` 改为调用 `view.catch_up` 报告 `EXPUNGE`、`EXISTS` 与 `FETCH FLAGS`，并新增 `Faults::garble_noop`。
- 文档与测试：更新 `docs/architecture/sync.md`、`docs/architecture/protocol.md`；补充 `connection.rs`、`imap_session.rs` 和 `watch.rs` 的测试（含 NOOP 连续报变化时的降速与恢复测试），运行 `cargo fmt`、`cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo test --workspace` 验证。
