# 在 catmail-core 的 test-support feature 中共享本地假服务器测试辅助函数

- 日期：2026-11-22
- 类型：重构

## 背景

`catmail-core`、`catmail-cli`、`catmail-mcp`、`catmail-gui` 四个 crate 针对 `catmail-testkit` 假服务器的测试里各自写了一份构造 `127.0.0.1` 明文 `ServerConfig` 与用 `Verify::Skip` 添加并同步账户的准备代码（#372）。`NewAccount` 或添加账户参数一变，四处都要跟着改。

## 决策

- **复用 `catmail-core` 已有的 `test-support` feature，不新建测试 crate**。辅助函数只接收端口和 `&MailClient`，不依赖 `catmail-testkit`，不会引入循环依赖，也不必增加 workspace crate。各调用方仅在 `[dev-dependencies]` 开启 `test-support`（workspace `resolver = "3"`，不影响正常构建）。
- **不把假服务器和 `MailClient` 创建一起包进来**。四个 crate 的服务器与客户端搭建方式差异较大（`catmail-core` 定制 builder 和 `reopen`、`catmail-cli` 用 `CountingSecrets`、`catmail-mcp` 不起 SMTP 且配置/数据目录分离、`catmail-gui` 假服务器跑在独立 runtime 上且拿全局 `Turn` 锁），只抽公共的 `ServerConfig`、`NewAccount` 和“添加并同步”三小段最干净。
- **`local_account` 返回 `NewAccount`**，调用方按需覆盖 `display_name`（如 MCP、GUI 的 `Some("Alice")`）或 `email`，同时把 `NewAccount` 字段构造收口到一处。

## 方案

- 新增 `crates/catmail-core/src/test_support.rs`（`#[cfg(feature = "test-support")] pub mod test_support`），导出 `local_server`、`local_account`、`add_synced_account`。
- `catmail-cli`、`catmail-mcp`、`catmail-gui` 在 `[dev-dependencies]` 中为 `catmail-core` 开启 `test-support` feature。
- 替换四处重复实现：`crates/catmail-core/tests/{common/mod.rs,identities.rs,shut_down.rs}`、`crates/catmail-cli/src/{test_support.rs,cmd/compose.rs,cmd/outbox.rs}`、`crates/catmail-mcp/src/tests/mod.rs`、`crates/catmail-gui/src/tests/mod.rs`。
- 验证：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
