# 邮件列表和搜索按游标翻页

- 日期：2026-10-21
- 类型：重构

## 背景

Issue #62：`Messages::list` / `Messages::search` 按 OFFSET 分页，GUI 滚到底时用已显示的行数作
offset。上一页之后顶上来了新邮件，已显示的行被挤进下一页（追加时用 `HashSet<RowId>` 去重掩盖）；
顶上删掉了行，下一页起点后错，中间几行漏掉，要等随后的刷新补回。OFFSET 还要先走过 n 行，越翻越慢。

## 决策

- 按 `(date, id)` 游标翻页（`MessageCursor`），下一页是排在最后一行之后的行。不用 `uid`：本地移动
  过来、服务器还没给新 UID 的行 `uid` 为空。文件夹列表的次序随之从 `date DESC, uid DESC` 改成
  `date DESC, id DESC`，和搜索一致。
- 没有采用 issue 里手写的 NULL 条件（`date < :d OR … OR date IS NULL …`）：查询计划显示它只用到
  `folder_id=?`，仍从文件夹第一行走起，和 OFFSET 一样慢。改为虚拟生成列
  `list_date = ifnull(date, i64::MIN)`（从不为空，没有日期的排最后），索引
  `messages_folder_date (folder_id, list_date)` 倒着读（索引末尾自带 rowid，再列 `id` 只会多存一份，
  索引大三分之一）。没有游标时绑定 `(i64::MAX, i64::MAX)`。
- 文件夹的一页分两次定位：游标日期里 `id` 更小的（`list_date = ? AND id < ?`），不够再取更早的日期。
  起初写成一个行值条件 `(list_date, id) < (…)`，review 指出 SQLite 只用 `list_date` 定位、逐行跨过
  同日期里 `id` 更大的行：2 万封同一时间的邮件翻到末尾要 26 万 VM 步，OFFSET 只要 8 万。把索引改成
  显式 `(folder_id, list_date, id)` 或 DESC 也一样；`UNION ALL` 两段能定位但计划里多出临时 B 树。
  两条普通语句最简单，每页约每行十来步，与深度无关。搜索不走索引（FTS 取出全部匹配再排序），仍用一个
  行值条件。
- CLI 的 `--offset` 改成 `--after <邮件 id>`（上一页最后一封的 id），由 CLI 读出这封邮件得到游标。
- 日期相同时按 `id`（存入先后）排，而首次同步先取最新的一批，跨批次的同秒邮件次序不再等于 UID
  次序。真实邮件很少同一秒；但假服务器所有邮件原来都是同一时间，测试的"最新在前"其实在测并列次序。
  改为假服务器按 UID 每封晚一秒（`time_of`），让测试表达真正的意图；要找某一封的测试按主题或 UID 找，
  不靠列表次序。

## 方案

- `catmail-types`：`MessageCursor`、`MessageSummary::cursor`；`SearchQuery::offset` → `after`。
- `catmail-store`：`0001_init.sql` 加 `list_date` 并改索引；`messages.rs` 的 `NEWEST_FIRST`、
  `cursor_params()` 供 `list` 和 `search` 共用，`list` 用 `LIST_SAME_DATE` / `LIST_OLDER`，`search`
  用 `AFTER`。
- `catmail-core`：`list_messages(folder, limit, after)`。
- `catmail-gui`：`message_list` 追加时传最后一行的游标，删除去重；刷新（从头重读已显示的行数）不变。
- 文档：`docs/architecture/storage.md` 新增"列表顺序与分页"，`gui.md` 更新分页说明。
- 测试：`pages_follow_the_last_row_across_equal_and_missing_dates`（同日期、无日期跨页，列表与搜索
  一致，翻页之间顶上新增和删除行后下一页仍接在最后一行之后）、`a_page_seeks_to_its_cursor`
  （2000 封同日期邮件，翻到末尾时 `list` 的 VM 步数不超过开头的两倍；退回行值条件时是 24055 比 72）。全部测试、clippy 通过；本地跑了 Dovecot / submission 测试。
