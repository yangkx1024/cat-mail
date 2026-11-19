# 搜索复用 ListQuery 的分页和过滤

- 日期：2026-11-19
- 类型：重构

## 背景

issue #360。#359 给列表加了 `ListQuery { limit, after, filter }`，`SearchQuery` 里仍各有一份同样的三个字段。
"搜索与列表用同样的分页和过滤"只是约定：调用方分别设置、自己保持一致，frontend 的 `list::fetch` 在 Search
分支里把三个字段逐个抄进 `SearchQuery`，MCP 和 CLI 的列表与搜索也各写一遍。

## 决策

- `SearchQuery` 带一个 `page: ListQuery`，`#[serde(flatten)]`：JSON 里 `limit`、`after`、`filter` 仍在顶层，
  格式不变（现在没有地方序列化它，但类型是 `Serialize`/`Deserialize` 的，测试固定了这一点）。
- 新增 `SearchQuery::paged(text, page)`；`SearchQuery::new(text)` 仍是前 50 条、不过滤。
- MCP 和 CLI 各加一个 `list_query` 辅助函数，按 `limit`、`after`、`only` 建 `ListQuery`，列表和搜索都用它。
  MCP 的搜索仍先查文件夹、再查 `after` 的邮件，报错的先后不变。
- 没有把 frontend `Fetch` 的 `limit`、`after`、`filter` 也换成一个 `ListQuery`：`fetch` 里已经只建一次，再往上
  改要动 `ListModel` 的读取逻辑，收益不大。

## 方案

- `catmail-types/search.rs`：`SearchQuery::page`、`paged`；测试 `a_search_keeps_its_page_beside_its_text`
  （序列化形式、往返、没有 `filter` 的旧形式）。
- `catmail-store/messages.rs`：`search` 读 `query.page.*`。
- `catmail-mcp/tools/reading.rs`、`catmail-cli/cmd/search.rs`：`list_query`。
- `catmail-frontend/list.rs`：Search 分支 `SearchQuery::paged(text, query)`。
- 文档：`mcp.md`、`gui.md` 的过滤说明。
- 验证：fmt、clippy、全部测试（沙箱外）。
