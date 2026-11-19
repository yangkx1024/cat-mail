# 统一列表查询的过滤参数，MCP / CLI 按标记过滤

- 日期：2026-11-19
- 类型：重构 | 功能

## 背景

issue #357。视图过滤（PR #358）在引擎里做，但各层接收过滤条件的写法不一致：`MailClient` 上每种列表两个
方法（`list_messages` / `list_messages_filtered`，`list_inboxes` / `list_inboxes_filtered`），store 的
`Messages::list` 转调 `list_filtered` 且只剩测试在用，`list_merged` 在签名里带 `&ListFilter`，搜索则是
`SearchQuery::filter` 字段。MCP 的 `list_messages` 工具和 CLI 调的是不带过滤的版本，agent 和脚本没法只列
未读或带旗标的邮件。

## 决策

- **一个查询结构体 `ListQuery { limit, after, filter }`**（`catmail-types`），与 `SearchQuery` 对齐：
  `Messages::list(folder, &ListQuery)`、`list_merged(folders, &ListQuery)`、`MailClient::list_messages(folder,
  ListQuery)`、`list_inboxes(accounts, ListQuery)`，去掉所有 `_filtered` 版本。放弃的方案：每个方法多一个
  `ListFilter` 位置参数，那样参数有四个，与搜索的写法仍是两套。`ListQuery::new(limit)` 是不带过滤的第一页，
  翻页用 `ListQuery { after, ..ListQuery::new(limit) }`。
- **对外只暴露 `flags`，不暴露 `also`**：`also` 是界面让刚读过的行留在 Unread 里的手段，agent 和脚本用不上。
- **MCP、CLI 各有一份枚举**（`Only`），转成 `FlagFilter`：`catmail-types` 是纯数据，不为 MCP 的 schema 加
  `schemars`、不为 CLI 加 `clap`。取值用 `unread`、`read`、`flagged`、`unflagged`，与 MCP 输出里的 `unread`、
  `flagged` 字段和 `FlagFilter` 的 serde 名一致。
- **单选**：`FlagFilter` 表达不了"未读且带旗标"，界面够用，等 agent 真的需要再改成 mask。
- frontend 的 `ListModel::show(source)` 与 `filter(listing, filter)` 合成 `show(source, filter)`：换来源时 GUI
  传 `FlagFilter::All`（与原来一样回到 All），换视图时传当前来源。

## 方案

- `catmail-types/search.rs`：`ListQuery`。
- `catmail-store/messages.rs`：`list` 合并 `list_filtered`，`list_merged` 收 `&ListQuery`。
- `catmail-core/client.rs`：`list_messages`、`list_inboxes` 收 `ListQuery`；core 集成测试、CLI 的测试跟着改。
- `catmail-mcp/tools/reading.rs`：`list_messages`、`search` 加 `only`；工具快照更新。
- `catmail-cli/cmd/search.rs`：`catmail ls`、`catmail search` 加 `--only`；空结果说 "No unread messages in INBOX."。
- `catmail-frontend`：`list::fetch` 用 `ListQuery`，`ListModel::show(source, filter)`；`catmail-gui` 的调用跟着改。
- 文档：`docs/architecture/mcp.md` 记 `only`，`gui.md` 的过滤说明，README 加 `--only` 的例子。
- 验证：MCP 新测试 `listing_only_messages_with_some_flags`（单个文件夹、所有收件箱、翻页、搜索、错误取值），
  CLI 的 `only_lists_the_messages_with_some_flags`（参数解析与映射）；store 原有的过滤测试改用 `ListQuery`。
