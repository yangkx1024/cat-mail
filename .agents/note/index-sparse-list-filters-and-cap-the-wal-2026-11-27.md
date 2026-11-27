# 稀疏的列表过滤走自己的索引，搜索先排 id，WAL 设上限

- 日期：2026-11-27
- 类型：重构

## 背景

#399 记下发布前 schema / 查询审查里没做的几项。这次做其中三项（3、4、5），其余看过后不改。数字来自
`0001_init.sql` 造的合成库（55 万封邮件、4 万封正文、2.1 GB），Python 自带的 SQLite 3.54.1，文件缓存是热的。

- 3：删除账户时 `secure_delete` 把释放的页全写进 WAL，checkpoint 不截短文件，WAL 在进程运行期间一直那么大。
- 4：Unread / Flagged 匹配不足一页时，`Messages::list` 把列表的索引读到头：20 万封的文件夹 44 ms，Gmail 10 万封
  的 INBOX 标签 33 ms，每次事件重读都是。
- 5：`Messages::search` 带着摘要（标签子查询、地址列）给全部命中排序，常见词命中 10 万封时 104 ms。

## 决策

- 3：`PRAGMA journal_size_limit` 64 MiB。实测 checkpoint 之后下一次提交把 124 MB 的 WAL 截回 67 MB；没有上限时不变。
- 4：Unread 加部分索引 `messages_unread (folder_id, list_date) WHERE flags & 1 = 0`；Flagged 复用"已加星标"的
  `messages_flagged`（按账户），不再加一棵按文件夹的。Gmail 标签从账户的"所有邮件"里找，再查标签行，不把 flags
  冗余进 `message_labels`（那要多一组触发器维护）。条件写成字面量，partial index 才用得上；`also` 拆成
  `UNION ALL` 的另一路，按 id 查（`CROSS JOIN` 固定先读 id 列表，否则 SQLite 可能去读文件夹的索引）。
  All、Read、Unflagged 取的是大多数行，照旧。代价：一半未读时批量插入慢约 7%，真实邮箱里更少。没解决：账户里
  未读的大多不在这个列表里时（"所有邮件"里几万封未读订阅、INBOX 里只有几封）仍要读完它们。
- 5：CTE（`MATERIALIZED`）先只取 id 和 `list_date` 排序截断，再为这一页取摘要。104 → 63 ms，命中少时一样。
- 不改的：
  - 6 搜索索引里删除后的残留：维持现状（`storage.md` 已写明）。
  - 7 `searched_on_server` 在正文全取完的文件夹上扫表：一次搜索一次，相对服务器搜索的网络时间不算多；要省得为
    每次插入多维护一棵按文件夹的部分索引，不值得。
  - 8 `Prefetch::next` 在剩下的都取不了时扫完索引：`skip` 只装服务器上重建了（新 UIDVALIDITY）的文件夹，很少见；
    `:discarded = 0` 那一遍每批多读一次剩下的已删除 / 垃圾邮件，每批本来就要走一次网络。
  - 9 `attachments.blob_hash` 的部分索引：GC 六小时一次，附件行小、整表扫得起；发布后加索引只是一个新迁移。

## 方案

- `crates/catmail-store/src/lib.rs`：`Store::open` 设 `journal_size_limit`；测试 `the_wal_has_a_size_limit`。
- `crates/catmail-store/migrations/0001_init.sql`：`messages_unread`（未发布，原地改）。
- `crates/catmail-store/src/messages/lists.rs`：`Page::sparse` 与 `UNREAD_IN_FOLDER`、`UNREAD_IN_LABEL`、
  `FLAGGED_IN_FOLDER`、`FLAGGED_IN_LABEL`，`list` 按过滤选；`search` 两步。测试
  `unread_and_flagged_page_like_the_whole_list`（文件夹、标签、"已加星标"，每种过滤带不带 `also`，一行一页翻完，
  与整张列表在 Rust 里过滤的结果比较，含同日期和没有日期的行），`pages_seek_in_their_index`（每种页的查询计划：
  用对索引，只有 `also` 那一路排序、按 id 查）。
- 实测（合成库，生成的 SQL 原样跑）：文件夹 Unread 44 → 0.5 ms、Flagged 44 → 0.8 ms；Gmail 标签 Unread
  33 → 0.5 ms、Flagged 33 → 1.0 ms；行数不变。
- 文档：`docs/architecture/storage.md`（SQLite、按标记过滤、搜索），`docs/architecture/gui.md`（视图过滤）。
