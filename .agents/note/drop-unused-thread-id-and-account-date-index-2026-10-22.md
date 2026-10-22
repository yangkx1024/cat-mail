# 删除未使用的 thread_id 列和 messages_account_date 索引

- 日期：2026-10-22
- 类型：重构

## 背景

两处死 schema（#39、#67）：

- `messages.thread_id` 列、`messages_thread` 索引和 `MessageSummary.thread_id` 从未被写入，只有两处
  构造时填 `None`。
- `messages_account_date (account_id, date DESC)` 没有查询用到：按 `id` 取行走主键，搜索的
  `:account IS NULL OR account_id = :account` 无法用索引定位，级联删除走以 `folder_id` 开头的索引；
  而且排序键已是 `list_date`，这条索引和列表顺序也对不上。两条索引都让同步的每次插入、删除多改一棵 B 树。

## 决策

- 全部删除，不改形状保留。会话分组真做时大概率是 `X-GM-THRID` / `THREADID` 这类可空列，现在占着名字
  反而误导；按账户列邮件（统一收件箱）的查询出现时再按它的排序键 `(account_id, list_date)` 建索引。
- 项目未发布，直接改 `0001_init.sql`。

## 方案

- `0001_init.sql`：删 `thread_id` 列、`messages_thread` 与 `messages_account_date` 索引。
- `catmail-types` 删 `MessageSummary.thread_id`；`catmail-store` 的 `SUMMARY_COLUMNS` / `summary_from_row`
  及 core、gui 测试里的构造随之删除。
- `docs/architecture/storage.md` 记下“会话列等决定后再加”“索引只为现有查询而建”。
- 验证：fmt、clippy `-D warnings`、`cargo test --workspace` 全部通过；纯删除，无新增行为可测。
