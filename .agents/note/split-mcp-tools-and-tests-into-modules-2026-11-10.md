# 把 MCP 的 tools.rs、tests.rs 拆成目录

- 日期：2026-11-10
- 类型：重构

## 背景

`catmail-mcp` 加了整理、草稿两个层级之后，`tools.rs` 到了约 1460 行（20 多个工具、它们的参数和辅助），
`tests.rs` 约 1260 行，找一个工具或测试要翻很久。仿照 `catmail-gui` 的做法（按功能分目录，`mod.rs` 放共用的
部分，每个文件几百行）拆开。

## 决策

- **`tools/` 按层级分**：`reading.rs`、`organizing.rs`、`drafts.rs` 各放那个层级的工具、参数、常量和只有它用的
  辅助；下载预算的扣与退被读和草稿两边用，单独放 `downloads.rs`。共用的（`Mail`、`Scope`、`Failed`、`tier_of`、
  按范围找账户/文件夹/邮件、`logged`）和 `ServerHandler` 留在 `mod.rs`。按层级分和 `tier_of`、`Tier` 一致，加工具时
  知道放哪。放弃的方案：参数单独一个文件，工具和它的参数就分在两处了。
- **每个文件一个 router**：rmcp 的 `#[tool_router(router = read_tools, vis = "pub(super)")]`，`Mail::new` 里
  `Self::read_tools() + Self::organize_tools() + Self::draft_tools()`。rmcp 列出工具时按名字排序，快照不变。
- **`tests/` 按主题分**：`mod.rs` 放 `Fixture` 和请求的辅助，子模块 `use super::*`（与 `#[cfg(test)] mod tests`
  的惯例一样）；快照随测试移到 `tests/tools.snapshot.json`。
- 只搬动，不改行为：可见性按需放宽到 `pub(super)`，`MAX_FILE_BYTES` 只在 `drafts.rs` 里用，不再 `pub(crate)`。

## 方案

- `crates/catmail-mcp/src/tools/{mod,reading,organizing,drafts,downloads}.rs`，
  `crates/catmail-mcp/src/tests/{mod,server,reading,organizing,drafts,limits}.rs`；`http.rs` 的注释改指
  `tools::drafts::MAX_FILE_BYTES`；`docs/architecture/mcp.md` 的"代码"一节。
- 验证：拆前拆后去掉 `use`、空行后的代码行排序比较，只差 import、三个 router 的相加、`pub(super)` 和快照路径；
  测试数量不变（25 个经 HTTP 的测试，crate 共 38 个）且全部通过，快照未变；fmt、clippy 通过。
