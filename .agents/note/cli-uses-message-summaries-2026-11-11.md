# CLI 改用 message_summaries，删掉 message_cursor

- 日期：2026-11-11
- 类型：重构

## 背景

Issue #278。`catmail mv` / `label` 用的 `by_account` 对每个 id 调一次 `get_message`，
每次都读正文和附件列表，却只用到 `account_id`；一次给很多 id 就是 N 次往返。
`cursor_after` 是 `MailClient::message_cursor` 唯一的调用方，而 MCP 已经改用
`message_summaries(vec![id])` 再 `.cursor()`。

## 决策

- `by_account` 一次 `message_summaries` 取全部摘要。它不保证顺序、缺的直接不返回，
  所以按传入的 id 顺序遍历：既决定分组内的顺序，也报出第一个缺的 id。
- 重复的 id 照旧在分组里出现两次，不顺手改行为。
- `cursor_after` 也改用 `message_summaries`，`message_cursor` 从门面删掉；store 的
  `messages().headers()` 还有草稿在用，保留。报错统一成 "message {id} not found"。
- `compose.rs` 的 `from_for`（`reply` / `forward` 带 `--from`）同样只为拿账户读整封邮件，
  一并改掉。取单封摘要的地方有两处，在 CLI 里加了 `cmd::summary`，没有在门面上再加一个
  单封的方法（这次本来就是要让门面少一个方法）。

## 方案

- `crates/catmail-cli/src/cmd/ops.rs`：`by_account` 改写；新增测试
  `messages_are_grouped_by_account_in_the_order_given`（两个账户的 id 混在一起，
  分组和顺序正确；缺一封时报的是那一封）。
- `crates/catmail-cli/src/cmd/mod.rs`：新增 `summary`，`cursor_after` 用它；
  `crates/catmail-cli/src/cmd/compose.rs` 的 `from_for` 也用它。
- `crates/catmail-cli/src/test_support.rs`：抽出 `add_synced`（加一个账户并同步一次），
  `synced_with_smtp` 和新测试共用。
- `crates/catmail-core/src/client.rs`：删掉 `message_cursor`。
- fmt、clippy `-D warnings`、全部测试通过。
