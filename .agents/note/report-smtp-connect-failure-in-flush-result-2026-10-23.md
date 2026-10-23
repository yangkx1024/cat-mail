# flush_outbox 在结果里带上连不上 SMTP 的原因

- 日期：2026-10-23
- 类型：重构

## 背景

issue #76。`flush_outbox` 返回 `Vec<OutboxEntry>`，连 SMTP 失败（登录被拒绝、服务不可用）时只把
原因写进各条目的 `last_error`。CLI 要区分退出码 1（改账户才行）和 3（稍后重试），只能在 flush 之后
读 `account_status(account).send`。那是全局状态，以后有后台发信或并发 flush 时可能是别的发送留下的；
也和回放的 `ReplayReport.error` 不一致。

## 决策

- 返回 `FlushReport { entries, error: Option<ReportedError> }`，`error` 在 `Smtp::Down` 分支由那次
  连接的 `Error` 填，带 `transient`。
- 不让这种情况返回 `Err`：调用方会丢掉被推迟条目的结果。
- `outbox flush` 仍按账户记下"发不了"的原因（报错信息和 `unsent` 都要按账户判断），但来源改为每个
  账户自己的 `FlushReport`，不再读账户状态。

## 方案

- `catmail-types/src/outbox.rs`：新增 `FlushReport`。
- `catmail-core/src/send.rs`：`flush` / `run` / `pass` 改为填 `FlushReport`；`client.rs` 跟着改签名。
- `catmail-cli/src/cmd/outbox.rs`：`blocked()` 变成 `error.filter(|e| !e.transient)`；`outbox flush`
  从各账户的报告里取。GUI 只 `.map(drop)`，无需改动。
- 文档：`docs/architecture/sending.md` 写明 flush 的结果，`docs/architecture/cli.md` 退出码一节以
  `FlushReport.error` 为准。
- 验证：新增 `send.rs` 的 `a_flush_reports_why_the_server_was_unusable`（`reject_login` → `Auth` 且
  非暂时，`unavailable` → 暂时，条目都是 `queued`，恢复后 `error` 为空）；
  `outbox::exit_code_says_how_a_send_ended` 不变且通过。
