# 邮件列表按标记过滤视图，标题行不再跳动

- 日期：2026-11-19
- 类型：功能

## 背景

列表标题左边勾选框旁的下拉菜单原来是"按标记勾选"（Read、Unread……），用户希望它是视图过滤：选了之后列表只显示
这些邮件，勾选框再全选它们。另外几处界面问题：有勾选时右边的工具栏（30 px 高的按钮）把标题行撑高，整个列表
往下跳；工具栏靠右；勾选框与上面的同步按钮、下面的日期标题都没对齐。

## 决策

- 过滤放在引擎的查询里，而不是只过滤已读到的行：文件夹是分页读的，只过滤已加载的页，Unread 可能只剩几行，
  nsh 的 `on_load_more` 每个行数只触发一次，过滤后一页没增加行就再也不会读下一页。引擎侧过滤后分页自然跟着走。
- 重读时已显示的行一起列出（`ListFilter::also`）：否则在 Unread 里打开一封就被标为已读，下一次刷新它从列表里
  消失、读信区关掉。行一直留到换视图或换来源，和 Gmail 的过滤视图一样不会从指针下跑掉。
- 换来源（点文件夹、搜索）时回到 All，免得到了别的文件夹以为邮件不见了。
- 菜单按钮从单个下箭头换成漏斗图标（Heroicons `funnel`，描边和填充两版放进 `assets/heroicons`）：下箭头看不出是过滤。
  过滤时换成填充的漏斗、主色，后面跟视图名，一眼能看出列表不全。
- 原有 `MailClient::list_messages` / `list_inboxes` 保留（CLI、MCP、测试在用），新增 `_filtered` 版本；
  `SearchQuery` 加 `filter` 字段（serde 默认值，MCP 传入的旧查询不受影响）。
- 工具栏紧跟标题，占原来说明文字的位置；不替换标题。

## 方案

- `catmail-types/search.rs`：`FlagFilter`（mask/want 表示）、`ListFilter { flags, also }`，`SearchQuery::filter`。
- `catmail-store/messages.rs`：`FILTERED` 条件加到列表的两段分页查询和搜索里；`list_filtered`，`list_merged` 带过滤。
- `catmail-core/client.rs`：`list_messages_filtered`、`list_inboxes_filtered`。
- `catmail-frontend/list`：`Check` 换成 `FILTERS` / `filter_label`；`Listing::filter`，`ListModel::filter` 从头
  重显同一来源；`Fetch` 带 `ListFilter`，重读时把已显示的邮件放进 `also`；`empty_text` 按过滤说 "No unread messages" 等。
- `catmail-gui/mailbox/list/mod.rs`：菜单改为切换视图（当前项打勾），勾选框勾选列出的全部；标题行固定为
  `Size::Bar` 的高度，左内边距 8 px（与日期标题文字、同步图标的字形同在 20 px 处），工具栏不再 `justify_end`。
- 测试：store 的 `lists_and_searches_only_what_the_filter_takes`（各过滤、分页、`also`、合并、搜索），
  frontend 的 `a_filter_shows_the_source_again_and_keeps_the_rows_shown_when_read_again`。
