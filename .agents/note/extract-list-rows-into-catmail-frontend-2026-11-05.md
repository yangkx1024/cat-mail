# 列表的行抽进 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

nsh 版要做邮件列表（`crates/catmail-nsh/ROADMAP.md` 的 P3）。gpui 版 `mailbox/message_list` 里来源怎么读、
按天分组、重读后选中去哪、按标记勾选和行上的文字都与 GPUI 无关，按"纯逻辑共用，不复制"先抽出来。

## 决策

- 新增 `catmail_frontend::list`：`Row`、`RowId`、`fetch`（各来源分页读）、`Days`（按天分组；GUI 的 `IndexPath`
  换成 `(section, row)`，由 GUI 自己转换）、`Reselect` / `reselect`、`Check`、`label_names`（对标签名的类型泛型，
  GUI 仍用 `SharedString`）、`recipients`、`draft_recipients`、`message_label`、`outbox_status` / `Severity`、
  `empty_text`、`avatar_hue`、`initial`，以及 `PAGE`、`SEARCH_DELAY`、`SERVER_SEARCH_DELAY`、`ROW_LABELS`。
- `catmail_frontend::mailbox` 加 `Place::folders`（原 `Mailbox::shown_folders` 的分支）、`list_title`、
  `list_note`（列表标题和旁边的"N unread"、发件箱的提示）。
- 头像的底色由主题给（`ThemeColors::avatar`），只把按地址挑第几个色相搬出来。
- 行怎么画、`MessageList` 的选中与滚动、悬停的命令按钮留在 GUI。

## 方案

- `catmail-frontend/src/list.rs`（新）、`mailbox.rs`；GUI 的 `message_list/{mod,rows}.rs`、`mod.rs`、`panes.rs`、
  `outbox.rs`、`layout.rs`、`theme/colors.rs` 改为引用，行为不变。
- 测试：按天分组、重选、收件人随代码搬进 catmail-frontend，另加标签名、头像色相的测试。
- 验证：`cargo test`（frontend、gui）、workspace clippy、fmt 通过。
