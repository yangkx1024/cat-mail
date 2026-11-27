# 发布前定下 schema：列的顺序、auto_vacuum、先存下的邮件头

- 日期：2026-11-27
- 类型：重构

## 背景

发布之后 `0001_init.sql` 不能再改。发布前把 schema 和查询过了一遍（用真实的迁移造了 55 万封邮件、4 万封
正文、2.1 GB 的库实测），列表、标签、计数触发器、外键索引都没有问题，只有三处是以后要重建表、整库
`VACUUM` 或让用户重新同步才能改的：

- 大文本列排在小列前面。SQLite 读排在溢出内容之后的列，要顺着溢出页链一页页走过去：
  - `message_bodies` 的 `raw_blob_hash`、`content_version` 在正文之后，GC 每次列出引用都把整张正文表读一遍
    （237 ms、读 1.8 GB，文件缓存是热的）；
  - `drafts` 的 `revision`、`remote_*`、`upload_*` 在 `quote_html` 之后，草稿列表和 `need_upload` 每次都走过
    每篇草稿的 HTML（30 篇、各引用 380 KB：1.48 ms 和 1.27 ms）。表里注释说的"列表不读 HTML"没有做到。
- 删除腾出的页留在文件里：删掉一个账户后 660 MB 的库仍是 660 MB，其中 613 MB 空闲，永远不还给磁盘。
- 同步取到了 INTERNALDATE、`X-GM-THRID`、In-Reply-To，却没有存。

## 决策

- **大文本列放到表的最后**，HTML 又排在纯文本之后，写进迁移开头的约定和 `storage.md`。`messages` 的行
  很少超过一页，不动。
- **GC 的引用另加部分索引** `message_bodies_raw (raw_blob_hash) WHERE raw_blob_hash IS NOT NULL`：只换列序是
  37 ms、读 164 MB（每行留在叶子页里的那部分），加上索引 7.6 ms。索引以后也能加，顺手一起做了。
  `attachments.blob_hash` 没加：行小，整表扫得起。
- **`auto_vacuum=INCREMENTAL`**，在 `Store::open` 里先于一切写入设置：只对空库生效，所以不放在迁移里。
  `FULL` 每次提交都搬页，不取。空闲页由 `Store::release_free_pages` 交还，随 GC 运行，不另起任务；每次
  16 MiB、各自一个事务，删掉大账户后不会长时间占着写连接。不设阈值：平时腾出的页不多，交还它们花不了
  什么。交还了多少只记日志，`GcReport` 仍只说 blob。
- **三列先存下，不用**：`gm_thrid`、`in_reply_to_hdr`、`internal_date`。加列以后便宜，回填要每个用户把
  所有邮件头重新拉一遍。`date` 的含义不变（Date 头，没有时才用 INTERNALDATE），列表排序不变；不为它们建
  索引，`THREADID`（RFC 8474）协议层能取到但 `FetchedMessage` 里还没有，这次不加。
- **没有改的**（都不涉及表结构，以后随时可以做）：
  - `messages` 的 `UNIQUE (id, account_id)` 只为 `message_labels` 的复合外键存在，约占批量插入的 10%；
    同步受网络限制，留着数据库层面的保证。
  - 未读 / 已加星标过滤在匹配不足一页时扫完整个文件夹（20 万封 44 ms）。部分索引能到 0.4 ms，但
    `OR m.id IN (:also)` 要拆成 UNION，Gmail 的标签也用不上。
  - 搜索常见词（10 万封命中 104 ms）可以先取 id 排序截断再取摘要（63 ms）。
  - `journal_size_limit`：删账户时 `secure_delete` 把释放的页全写进 WAL，WAL 涨到 619 MB，进程运行期间
    不会变小。
- **搜索索引的删除残留只写进文档**：`contentless_delete` 删一封邮件只记墓碑，它的词留在索引段里直到
  合并（实测确认），`secure_delete` 管不到。接受，`storage.md` 写明。

## 方案

- `crates/catmail-store/migrations/0001_init.sql`：`message_bodies`、`drafts` 换列序；`messages` 加三列；
  新索引 `message_bodies_raw`；开头的约定加一条。
- `crates/catmail-store/src/lib.rs`：`Store::open` 设 `auto_vacuum`；`Store::release_free_pages`。
- `crates/catmail-store/src/messages/sync.rs`：`NewMessage` 加 `in_reply_to`、`gm_thrid`，`upsert` 存三列。
- `crates/catmail-core/src/sync/mod.rs`：`new_message` 带上它们。`engine/storage.rs`：GC 之后交还空闲页。
- 文档：`storage.md`（SQLite、邮件行、BlobStore）、`labels.md`、`sync.md`。
- 测试：`free_pages_are_released`（删掉账户后文件按交还的字节数变小，库完好；没有设 `auto_vacuum` 时
  失败）、`blob_references_of_bodies_come_from_their_index`（查询计划）、
  `keeps_what_sync_fetched_and_nothing_reads_yet`、`a_new_message_keeps_its_thread_and_when_it_was_received`。
- **已有的本地库要删掉重新同步**：它的 `user_version` 已经是 1，迁移不会再跑，缺新列的库同步时会报
  `no such column`。
