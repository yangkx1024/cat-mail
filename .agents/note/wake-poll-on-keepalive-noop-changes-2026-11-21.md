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

- **让 `Session::noop()` 返回 `Result<bool>` 并与 `IDLE` 同样解析 `changed_flags`**：NOOP 本身不会产生自己的 FETCH 响应，因此 NOOP 期间的 `FETCH` 与无法解析的行都可以像 IDLE 一样视为变化；没有变化或开启 CONDSTORE 时不发额外命令，无额外往返。`resolve_changed_flags` 改为向 `Session::changed_flags` 合并而不是覆盖，避免退避期间多次 NOOP 丢失前面记录的 UID。
- **严格限定提前结束等待的范围**：只有正常轮询且 INBOX 处于选中状态（`Wait::Poll` 以及因 `!idle_works` 降级轮询的 `Wait::Idle`）才在 NOOP 报告变化时提前退出等待；退避（`busy > BUSY`）、INBOX 同步被拒（`Wait::InboxFailed`）及无 INBOX（`Wait::NoInbox`）均不提前唤醒，以免破坏退避策略或把其他文件夹的变化当成 INBOX。

## 方案

- `crates/catmail-proto/src/imap/connection.rs`：新增 `Connection::noop()`，与 `IDLE` 共用变化追踪模式（`Mode::Noop`），把 NOOP 中的 `FETCH` 和畸形行计入 `changed` / `changed_flags`。
- `crates/catmail-proto/src/imap/session.rs`：`Session::noop()` 返回 `Result<bool>`（未选中文件夹时返回 `false`），有变化时调用合并式的 `resolve_changed_flags()`。
- `crates/catmail-core/src/pool.rs`：`Pinged::Alive` 携带 `changed: bool`。
- `crates/catmail-core/src/watch.rs`：区分 `Wait::Poll` 与 `Wait::NoInbox`，在等待循环里根据 `stop_on_change` 决定是否在 `KeepAlive::Changed` 时提前结束等待，并在 INBOX 选中时将 `changed_flags` 合并保留。
- `crates/catmail-testkit/src/imap/mod.rs`：假服务器的 `NOOP` 改为调用 `view.catch_up` 以报告 `EXPUNGE`、`EXISTS` 与 `FETCH FLAGS`。
- 文档与测试：更新 `docs/architecture/sync.md`、`docs/architecture/protocol.md`；补充 `connection.rs`、`imap_session.rs` 和 `watch.rs` 的测试（并调整 `keeps_the_connection_open_between_polls` 使其在提前唤醒下仍确实越过 `idle_timeout`），运行 `cargo fmt`、`cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo test --workspace` 验证。
