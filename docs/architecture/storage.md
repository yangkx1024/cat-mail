# 存储

[← 架构](../architecture.md)

## SQLite

- `rusqlite`（bundled：三平台行为一致、自带 FTS5），WAL，`foreign_keys=ON`。
- 1 写 + 读池：`Store::write` 走唯一的写连接（Mutex），`Store::read` 从 4 个只读连接的池里借一个，
  都借出时等别的读归还。WAL 下读不必排在同步每批 200 条的写事务后面；快照在闭包的第一条查询时取得
  （`DEFERRED`），闭包内所有查询看到同一份已提交的状态，至少包含调用之前的所有提交。
  只读连接以 `SQLITE_OPEN_READ_ONLY` 打开，`read` 闭包里误写会直接报错；在迁移之后随 `Store::open`
  一起打开，不会看到旧 schema。没有内存数据库：另开的连接会是另一个空库，读池无从谈起，测试也用临时
  目录里的文件。闭包不得回调同一个 `Store` 的规则不变：写里套写会死锁，写里套读看不到未提交的改动，
  读里套读可能把池耗尽。
- `secure_delete=ON`：删除账户、UIDVALIDITY 重建、GC 之后，正文不会留在空闲页里等着被覆盖。代价是
  被释放的页都要（经 WAL）写一遍零，包括 FTS 合并释放的。没有用 `FAST`：它不清零进入空闲列表的溢出页，而大段正文正在溢出页里。
  - 它管的是被释放的页，管不到搜索索引里还在用的页：`contentless_delete` 删除一封邮件只记一个墓碑，
    它的词（连同位置）留在所在的索引段里，搜不到，但字节还在文件中，等这个段被合并时才消失。删掉的
    多了（删除账户、UIDVALIDITY 重建）FTS5 会很快合并这些段；零星删掉的一封要等后面的写入带来合并，
    没有期限（实测：删一封后词仍在文件里，再存入一万封后消失）。目前接受：要立刻清掉只能
    `'optimize'` 重写整个索引。正文、主题这些存在表里的原文不受影响，随删除清零。
- `journal_size_limit` 64 MiB：checkpoint 只把 WAL 的内容写回库，不截短文件，WAL 一直保持自上次截短以来最大
  那个事务撑到的大小，直到进程退出。删除一个账户时被释放的页都清零写进 WAL（实测 660 MB 的库删一个账户，WAL
  涨到 619 MB），`release_free_pages` 搬页也经过 WAL，GUI 一开就是很多天。有了上限，checkpoint 之后的第一次
  提交把文件截回 64 MiB（实测 124 MB → 67 MB；没有上限时保持 124 MB）。
- `auto_vacuum=INCREMENTAL`：SQLite 默认把删除腾出的页留在文件里等以后的写入用，文件只增不减，删掉一个
  账户，它占的空间永远不还给磁盘。这个模式多记一份每页归谁的指针表，平时不搬动任何页；
  `Store::release_free_pages` 把空闲页交还文件系统（每次 16 MiB、各自一个事务，中间让别的写入先走），
  随 [GC](#blobstore) 一起运行。
  - 必须在新库写入任何内容之前设置（`Store::open` 里先于 WAL），已有内容的库要整库 `VACUUM` 才能换上：
    这是发布前定下来的，不要挪到迁移里或建表之后。没有设置它的旧库上 `release_free_pages` 什么也不做。
- 迁移在 `crates/catmail-store/migrations/`，由 store 自己的 `migrate::to_latest` 执行：
  `user_version` 记库已经跑过几个迁移，没跑过的按顺序在一个事务里跑完，提交前 `foreign_key_check`，
  失败就整个回滚；库的版本比程序认识的新（较新的 catmail 迁移过）就拒绝打开。
  发布之后迁移文件不可修改，改 schema 一律新增文件，迁移不能重排、删除。
  - 计数与 `rusqlite_migration` 相同，两者可以互换：这是约束，不要改成别的记法。
  - 迁移在事务里，`PRAGMA foreign_keys` 在其中无效。重建表的迁移关不掉外键，弄坏的引用由提交前的
    检查拦下。
- 约定：时间为 unix 秒；枚举列为 TEXT 不加 CHECK（新增变体无需重建表）；地址列表为 JSON；外部可见
  的 ID（accounts / folders / messages / drafts）用 `AUTOINCREMENT`，删除后不复用。
- **大文本列放在表的最后**（`message_bodies` 的正文，`drafts` 的正文、引文，HTML 又排在纯文本之后）。
  一行超过一页时后半截存在溢出页里，SQLite 要读排在它后面的列，得顺着溢出页链一页页走过去：小列排在
  前面，读它们就不碰正文。排在后面时，草稿列表和上传检查每次都要走过每篇草稿的 HTML，GC 列出正文引用的
  blob 要把整张正文表读一遍（4 万封、各 38 KB HTML 的库：读 1.8 GB；换序后 164 MB，加上
  `message_bodies_raw` 索引只读索引本身）。列的顺序发布后只能重建表才能改，新表照此排列；以后
  `ADD COLUMN` 加的列只能排在最后，会被经常读的小列若落在大文本后面，给它建索引（如
  `message_bodies_version`）。`messages` 的行很少超过一页，不讲究。

## Repository 与级联

`Store::read` / `Store::write` 的闭包拿到一个 `Db`，它把各个 repository（`db.folders()`、
`db.drafts()`……）绑在同一个事务上，跨表的改动要么全部提交，要么全不提交。repository 之间的调用只有一个
方向：

- **查询顺着外键**：一个 repository 只调用它的行引用的那些（草稿查身份和文件夹，操作查邮件和文件夹，邮件
  查文件夹），从不调用引用它的。例外是附件：它是正文的一部分，`messages` 存正文时一并写附件，
  `Attachments` 不认识 `messages`。SQL 照样可以 JOIN 别的表，这条规则管的是 repository 之间的调用。
- **级联在上面一层**：一处改动要接着改到另一个 repository 管的东西时，写成 `Db` 的方法（`cascade.rs`），
  依次调用各方，各方只做自己那部分。例如删除文件夹（`Db::remove_folder`）要带走按它的 UID 记的操作和草稿
  副本，删除身份（`Db::delete_identity`）先把草稿交给账户的默认身份，发完的草稿随发件箱条目一起删除
  （`Db::finish_outbox`）。要知道一处改动会波及哪些表，看 `cascade.rs` 就够了。
- **大家都用的在下面**：`Touched`（一处改动改了哪些列表，给事件用）、`columns`。

```text
Db 的级联方法（cascade）
  ↓
drafts → identities、folders
ops → messages、folders
messages → folders、attachments
accounts、outbox、prefetch
  ↓
touched、columns
```

目录：

- `messages/`：邮件行，和挂在邮件行上的东西。`Messages` 分三个文件：同步存进来、改动和删除的（`sync.rs`），
  列表、搜索、读一封邮件和新邮件（`lists.rs`），正文、预览、搜索索引和从服务器哪里取（`bodies.rs`）。同一目录里
  还有正文的附件（`attachments.rs`）、搜索索引的分词（`fts.rs`，只有邮件用）、正文预取记下的东西（`prefetch.rs`）。
- `ops/`：本地改动和回放的队列，见[操作队列](op-queue.md)。
- 其余的 repository（账户、身份、文件夹、草稿、发件箱）、级联（`cascade.rs`）、`touched.rs` 和 `columns.rs`
  在 `src/` 下，连同数据库本身（`lib.rs`、`migrate.rs`、`error.rs`）、blob 和只给用户读写的文件（`blob.rs`、
  `private.rs`）。

## 邮件行

一行是服务器上的一封邮件：它的位置（`kind = 'location'` 的文件夹）和那里的 UID，见
[架构](../architecture.md#一行是服务器上的一封邮件uid-绑定-uidvalidity)。Gmail 上一行是一个 `X-GM-MSGID`
（`gm_msgid`，账户内唯一），其余邮箱是标签（`message_labels`），见[位置与标签](labels.md#存储)。补充：

- 移动得到新的 UID：服务器给出 COPYUID 时同一行改为新的（文件夹, UID），否则旧行删除、同步时成为
  新的一行。本地移动过来、服务器还没移动的行 `uid` 为空。
- 普通账户上不同文件夹里的副本不关联：`EMAILID`（RFC 8474）相同的副本 flags 仍各自独立，只能用于分组。
- **先存下、还没有用到的列**：`gm_thrid`（`X-GM-THRID`）、`in_reply_to_hdr`、`internal_date`
  （INTERNALDATE 本身；`date` 仍是 Date 头，没有时才用它）。同步本来就取到了它们，存下只多几十个字节；
  不存的话，以后要用就得让每个用户把所有邮件的头重新拉一遍，加列本身反而便宜。会话分组怎么做还没定
  （Gmail 之外要靠 In-Reply-To / References，或 `THREADID`，后者协议层能取到、还没存）；列表现在按 Date
  头排序，它由发件人填写，日期写在未来的邮件会一直排在最前，要改成按收信时间或拿它修正，靠的是
  `internal_date`。现在没有查询读这三列，也没有为它们建索引。
- 索引只为现有查询而建：每条索引都让同步的每次插入、删除多一棵 B 树要改。按账户整体列出邮件的索引已
  删除，多账户合并收件箱（All Inboxes，`list_inboxes`）复用各收件箱的文件夹 / 标签索引分页。

**计数。** `folders.unread_count` / `total_count` 由 `messages` 上的触发器维护（插入、删除、
`\Seen` 变化各 O(1)），不论变化来自同步、本地操作还是级联删除，写入方都不用记得刷新。标签的计数由
`message_labels` 和 `messages` 上的另一组触发器维护，每次变化 O(标签数)，见
[位置与标签](labels.md#存储)。

**跨账户约束。** 引用其他账户内数据的行用 `(id, account_id)` 复合外键（`messages`、`pending_ops`
→ `folders`），数据库层面就不可能把账户 A 的邮件挂到账户 B 的文件夹下。`ON DELETE SET NULL` 的关联
（如 `drafts.reply_to_message_id`）没法用复合外键，因为 SET NULL 会连 `account_id` 一起置空，这类
关联由 repository 校验。

## 列表顺序与分页

文件夹列表和搜索结果都按日期倒序，日期相同时按 `id` 倒序（存入的先后），没有日期的排在最后
（`NEWEST_FIRST`）。翻页用游标（`MessageCursor`：最后一行的日期和 `id`），下一页是排在它之后的行：
键唯一且不变（`uid` 不行：本地移动过来的行在服务器给出新 UID 之前为空），所以上一页之后顶上新来或
删掉的行不会让下一页重复或漏行。

`date` 可以为空，而 SQLite 的行值比较遇到 NULL 得到 NULL。手写的 `date IS NULL OR …` 条件会让查询
放弃索引的范围查找，从文件夹的第一行走起，和 OFFSET 一样越翻越慢。所以排序键是虚拟生成列
`list_date`（`date`，没有日期时为 `i64::MIN`），从不为空。

文件夹的一页是 `messages_folder_date (folder_id, list_date)` 上的两次定位（倒着读；索引末尾本来就带着
rowid 即 `id`，不必再列一次）：先取游标那个日期里 `id` 更小的（`Page::same_date`，定位到
`folder_id, list_date, rowid`），不够一页再取更早的日期（`Page::older`）。一个行值条件
`(list_date, id) < (…)` 看似等价，但 SQLite 只用 `list_date` 定位，要逐行跨过同一日期里 `id` 更大的
所有行：几万封同一时间（或都没有日期）的邮件里，越往后翻每页越慢，比 OFFSET 还慢。没有游标时绑定
`(i64::MAX, i64::MAX)`。有测试比较翻到一组同日期邮件末尾和开头时 `list` 走的 VM 步数。

**按标记过滤。** All、Read、Unflagged 取的是大多数行，照上面在列表自己的索引上分页，读到一页就停。Unread、
Flagged 通常只取几行：不足一页时，列表的索引要读到头才知道没有更多了，20 万封的文件夹每次重读约 44 ms。
所以这两种换一棵只有这些行的索引（`Page::sparse`）：未读是部分索引 `messages_unread (folder_id, list_date)
WHERE flags & 1 = 0`，带旗标的用"已加星标"那棵 `messages_flagged`（按账户，再看文件夹）；Gmail 的标签从它
账户的"所有邮件"里找未读的 / 带旗标的，再查标签行。实测都在 1 ms 以内。

- 条件要在 SQL 里写成字面量（`m.flags & 1 = 0`），和索引的 `WHERE` 一样：绑定参数的 `flags & :mask = :want`
  SQLite 无从判断部分索引里有它要的所有行。
- `also`（视图里已显示、之后被改掉的行）若和过滤写成一个 `OR`，查询就退回列表的索引；所以是 `UNION ALL` 的
  另一路，先读 id 列表再逐个按 id 查（`CROSS JOIN`），只排序这几行。有测试看每种页的查询计划。
- 代价：`messages_unread` 让未读邮件的每次插入、删除、标记变化多改一棵 B 树。14 万封、一半未读时批量插入慢约
  7%（14.4 → 15.5 s）；真实邮箱里旧邮件几乎都已读，代价随未读的比例变小。
- 没解决的：账户里未读（或带旗标）的邮件大多不在这个列表里时，仍要读完它们：比如"所有邮件"里有几万封未读的
  订阅邮件，而 INBOX 标签里只有几封未读。这时和以前差不多。"已加星标"本身就是 `messages_flagged`，过滤照常。

搜索先由 FTS 取出全部匹配再排序，用不上索引，游标只是过滤，所以一个行值条件（`AFTER`）就够了。排序只带
id 和 `list_date`，截到一页之后才为这一页取摘要（标签子查询、地址这些列）：常见词命中 10 万封时 104 ms →
63 ms，命中少时一样。

## 全文搜索

`messages_fts` 为 contentless FTS5（`contentless_delete=1`），rowid = `messages.id`，列为主题、
发件人（姓名和地址）、收件人与抄送、正文。插入邮件时写入（正文为空），正文下载后在同一事务里删除
并重写这一行；删除邮件（含级联删除）由触发器删除索引。所以邮件头总能搜到，正文只有下载过的才能搜到；
开着[后台预取](bodies.md#后台预取)的前端会在后台把正文取下来，更早的由[问服务器](#问服务器)补上。

**中日韩分词。** unicode61 会把连续的 CJK 文字当成一个词，trigram 又不支持两个字的查询，所以在 Rust
侧预先切分（`crates/catmail-store/src/messages/fts.rs`）：一段连续的 CJK 文字写成相邻双字，后面再跟单字，
如 `季度报告` → `季度 度报 报告 季 度 报 告`。查询时一个字匹配单字，两个字匹配双字，三个字以上变成
相邻双字组成的短语（`"季度 度报 报告"`）；同一段的双字位置连续，所以短语恰好匹配原来的子串，不会
跨段误配。其他文字交给 unicode61（忽略大小写和变音符号，按整词匹配）；全角 ASCII 折叠成半角；混合
文字的词（`iPhone手机`）拆成几段分别匹配。

**查询语法**（`SearchQuery`）：空格分隔的词或 `"短语"` 都要匹配，可加 `from:`、`to:`（含抄送）、
`subject:` 前缀。每个词都放进 FTS5 字符串里，用户输入不会被当成 FTS5 语法。结果按日期倒序。

**每封邮件一条结果**：搜索的是邮件行，不按 Message-ID 合并（`SEARCHED`）。普通账户上不包括 `all`、
`flagged` 角色的文件夹：Dovecot 等可以配置虚拟的 `\All` / `\Flagged` 邮箱，只汇集其他文件夹，每封邮件在
别处都有一行（视图 `gathering`，问服务器时也用它）；用户指定在其中搜索时照常。Gmail 上"所有邮件"是位置，照常搜索；在标签或"已加星标"里
搜索，范围与它的列表相同（"所有邮件"里带这个标签、或带 `\Flagged` 的）。

### 问服务器

预取受每日预算限制，正文没下载的邮件按正文搜不到。`MailClient::search_server` 用同一段文字问服务器，返回
服务器找到、正文还没下载的邮件 id（`search::on_servers`）；前端把它们放进 `SearchQuery.found_on_server`，
`Messages::search` 里与 FTS 的匹配取并集（`OR m.id IN json_each(:found)`），账户、文件夹的范围照旧。

- **命中当过滤条件，不当另一批结果。** 考虑过返回补充的摘要让前端合并：补充的邮件日期散在各页，并进已显示的
  页会打乱顺序和下一页的游标；事件来了重读本地结果，补充的又丢了。现在排序、分页、事件后重读都走同一条 SQL，
  引擎也不用缓存搜索结果。正文已下载的不要：本地 FTS 为准。每个文件夹最多取正文没下载的里 UID 最高的 1000 封（先
  过滤再截断：否则最新的 1000 封正文都在时，要补的旧邮件一封也留不下），一个到处都有的词不至于让每次读结果都
  带上几万个 id。
- **范围与本地搜索相同**（`Folders::searched_on_server`，账户和文件夹范围都在 store 里定）：
  - 普通账户搜每个可选中的位置（排除 `gathering`）；Gmail 搜“所有邮件”、垃圾邮件、已删除三个位置；指定
    文件夹时只搜该文件夹，Gmail 的标签和“已加星标”搜“所有邮件”。没有正文缺失邮件的文件夹直接跳过，省下
    两个来回和服务端扫描。
  - 按 `LOCATION_ORDER` 逐个只读 EXAMINE 再 `UID SEARCH`，UIDVALIDITY 与本地不同的跳过。搜索连接上已只读
    打开着的文件夹（上次最后搜的）排到最前、复用不重开（`Session::examined_mailbox`，条件见
    [协议 / 连接池](protocol.md#连接池)）：只搜一个文件夹时第二次起免 EXAMINE，Gmail 两三个位置每次少
    一次。
  - 结果 UID 在同一 UIDVALIDITY 下映射回本地行（`Messages::without_body`）；服务器有但本地尚未同步的
    忽略，交给同步。全部文件夹都搜时可能耗时几秒，但本地结果会先显示，不必等它。
- **查询翻译**（`Session::uid_search_text`）：查询解析在 `catmail-types`（`SearchTerm::parse`），本地和服务器
  共用。普通 IMAP 每个词一个键（隐含 AND）：`TEXT`，`from:` → `FROM`，`to:` → `OR TO … CC …`，`subject:` →
  `SUBJECT`；服务器按子串匹配。Gmail 用 `X-GM-RAW`，写成它自己的语法（`"词"`、`from:"…"`、`{to:"…" cc:"…"}`、
  `subject:"…"`），与网页版的搜索一致：按词匹配，中文先分词，词中间的片段（"长桥证券"里的"桥证"）和英文
  前缀都搜不到（#222 验证），本地 FTS 的双字切分能搜到；正文没下载的邮件 Gmail 补到的会少一些，可以接受。
  有非 ASCII 字符时加 `CHARSET UTF-8`，文字用字面量发送。
- **按问候语认出的服务器**（`TextSearch`，2026-11 在真实服务器上验证，见 #222）：
  - QQ 邮箱（含企业邮，问候语以 `QQMail ` 开头）无视 `TEXT`（`TEXT x`、`NOT TEXT x` 都返回整个文件夹），
    也不读 SEARCH 里的字面量，把 `{n}` 本身当成要搜的文字。所以不带字段的词写成 `OR BODY … OR SUBJECT … OR
    FROM … OR TO … CC …`（正好是本地搜索的几列），非 ASCII 文字用带引号的 8 位字符串（不合 RFC 3501，只对
    QQ）。英文按整词匹配，中文按子串。没有为它把所有服务器都改成 `BODY` 等的 OR：没有 LITERAL+ 的服务器上
    每个字面量都要多一个来回，`TEXT` 只要一个。
  - Coremail（网易 163、126 等，问候语以 `Coremail ` 开头）的 SEARCH 除了 Message-ID 什么文字都搜不到，也
    不报错，只回空结果：不问它（只在第一个文件夹 EXAMINE 之后就停），省下每个文件夹两个来回。按 Message-ID
    找邮件（发送重试前查已发送、草稿上传后定位）不受影响。
  - iCloud 是标准的（`TEXT`、`NOT`、字面量、`CHARSET UTF-8` 都正常，中文按子串、正文解码后搜），不特别处理。
    英文整词匹配，词只按空格和少数标点切开：正文里 `a.b.c` 中的 `a` 本地 FTS 能搜到（在 `.` 处断词），iCloud
    搜不到；QQ、Gmail 也是整词匹配，这类差别接受。
  - 兜底：一个文件夹的邮件全部命中时，用同样的写法再搜一次谁都没有的随机文字（UUID），也有命中就说明服务器
    无视文字，这个账户不再问。没用"把条件取反再搜"：QQ 连 `NOT` 也无视，正常的搜索会被误判。一条连接上通过
    一次就不再查（小文件夹常常全部命中，Gmail 上每次要 300 多毫秒）。
  问候语说不能按文字搜的，连接时就记进 `accounts.searches_text`（`connection::connect`，同步、预取、发送、
  添加账户谁先连上谁记，每次连接重写，服务器换了问候语也会改过来），`searched_on_server` 不列出这个账户，
  搜索不连接、不登录，引擎重启后也一样（#223）。考虑过把连接和登录拆开、搜索先只看问候语，但那要改所有连接
  共用的入口，只为搜索一个调用方。对照搜索发现的无视文字只记在 `SearchSlot` 里，引擎运行期间不再问：它不是
  问候语认出来的，不存进数据库。
- **服务器不支持也不算错。** 拒绝 SEARCH（`BADCHARSET` 或 NO / BAD）就不再搜这个账户的其他文件夹（每个都会拒）；
  打不开的文件夹跳过；连不上、断线只记日志。登录被拒或超限的账户不去问（每次都算一次失败的登录）：同步记下的看
  `Engine::waits_out`，搜索自己遇到的（同步之后密码才失效）记在 `SearchSlot` 里，等之后一次完整同步成功才再问，和预取
  一样。前端拿到的
  永远只是"多了哪些"，离线时就只有本地结果。Dovecot 没装全文搜索插件时按子串搜，中文、GBK 正文都能搜到（真实
  Dovecot 测试覆盖）。
- **连接**：每个账户一条搜索专用的连接（`SearchSlot`），不用连接池（池里两条留给打开邮件）。空闲 2 分钟关掉，
  连着搜几次不用每次登录。调用方丢弃 future 就是放弃这次搜索（`search::Abandoned`，`Runtime::run` 的任务不随
  future 取消，所以由丢弃时的 guard 置位）：引擎做完手上的命令就停（EXAMINE 之后、开始搜正文之前也看一次，没有索引的
  服务器上那是扫一遍整个文件夹；全部命中时的随机文字对照之前同样看一次），把连接让给下一次；不中途丢弃连接，免得每停
  一次输入都重新登录。考虑过引擎里一个"最新搜索"计数让新搜索作废旧的：那样同一引擎的两个调用方（第二个窗口、
  以后的 IPC 前端）会互相取消，"哪次搜索是当前的"也要在前端和引擎各记一份。账户删除时取消并关掉连接。
- **提前连接**（`MailClient::connect_for_search`，`search::connect_ahead`）：
  - 第一次搜索的大头是登录（日志里 Gmail 约 1.8 s、QQ 约 0.5 s，EXAMINE + SEARCH 每个文件夹约 0.8 s），
    本地结果显示后还要等 1 秒才问服务器，正好用来提前登录。
  - 为 `searched_on_server` 会问到的账户登录进 `SearchSlot`，登录期间持锁：此时开始的搜索直接等它连好
    复用，不另开连接。连接走同一个 `pool::on_session`（已开着的不动，睡前建的换掉；正在被搜索占用的
    `try_lock` 跳过；登录被拒或超限同搜索一样记录；不支持文字搜索的服务器在连接时已写入库，不再列出）。
    无人使用时仍空闲 2 分钟关闭。
  - GUI 仅在搜索框获得焦点时触发（开始输入再连只有 1.2 秒，不够 Gmail 登录）；CLI 的 `search --server`
    立即查询，用不着提前连接。
- **前端**：GUI 的本地搜索结果显示 1 秒后再问服务器（`SERVER_SEARCH_DELAY`），结果到了、来源没变就重读一次
  （`ListModel::found_on_server`），之后翻页和事件重读都带上；失败时什么也不做。等这 1 秒是因为放弃的搜索仍要
  做完手上的命令（没有索引的服务器上是扫一遍整个文件夹的正文），下一次要等它：输入中的停顿（200 ms）不去问，
  代价是服务器结果晚到约 1 秒。只在按 Enter 时问也考虑过，但那样不再是默认就问。CLI 用 `catmail search --server`。

## BlobStore

内容寻址：`<data>/blobs/ab/cdef…`（SHA-256），相同内容只存一份。DB 只存 hash，没有 blob 表。

- 写入先进 `blobs/tmp`、fsync 后 rename，读者看不到半成品。
- rename 从不覆盖已有文件（`persist_noclobber`）：同一内容可能被两次下载同时写入，而 Windows 上覆盖
  一个正被别人覆盖或打开的文件会失败。目标已存在说明别人已经存好了，刷新 mtime 即可。

**GC 约定。** 从 `put` 返回到引用它的数据库行提交之间有一个窗口，而且 `put` 可能返回一个早已存在的
hash。因此只删除"没有被任何 `*_blob_hash` 列引用，**并且** mtime 早于宽限期"的文件，删除前再查一次
mtime。宽限期要远大于任何写事务的时长；`put` 遇到已存在的 blob 会刷新 mtime，宽限期重新计时。

**实现**（`BlobStore::collect_garbage`，由 `catmail-core` 的 `Storage::collect_garbage` 调用，入口
`MailClient::collect_garbage` / `catmail gc`）：先在一个
读事务里取出所有引用（`Db::blob_references`；有测试枚举所有 `*_blob_hash` 列，漏加会失败；正文的引用从
部分索引 `message_bodies_raw` 里读，不碰正文的行，也有测试看查询计划），再遍历
目录找出未引用且足够旧的候选，逐个在写锁下重查 mtime 后删除。`put` 刷新或创建 blob 时持有同一把锁
的读锁，"GC 判定为旧"与"`put` 刚刚刷新"不会交错。宽限期 1 小时；`tmp` 里中断写入留下的旧文件一并
清理。删除账户、UIDVALIDITY 变化等删除邮件之后，它们的 blob 由下一次 GC 回收，数据库文件里腾出的页也在
这时交还（`Store::release_free_pages`，见 [SQLite](#sqlite) 的 `auto_vacuum`；交还了多少只记日志，不进
`GcReport`）。

**谁来运行。** `catmail gc` 打不开别的前端正占着的数据目录，所以长期运行的前端让引擎定期运行
（`MailClientBuilder::collect_garbage_every`，`gc.rs`）：打开后最多 5 分钟第一次（不与启动时的同步
抢，也赶在只开一会儿的窗口关掉之前），之后按给定间隔。GUI 和 `catmail watch` 每 6 小时一次。

## 凭据

`SecretStore` trait，默认 `KeyringSecrets`（`keyring` 4：Linux Secret Service / macOS Keychain /
Windows Credential Manager），service 为 `catmail`，键为 `account:{email}:{随机 uuid}`，存在
`accounts.secret_key` 列。随机键保证不同数据目录的凭据互不覆盖。之后添加的发件身份（`identities`，见
[发信](sending.md#发件身份)）各有自己的密码，键为 `identity:{email}:{随机 uuid}`，规则相同；随账户建出的
那个身份没有自己的条目（`secret_key` 为空），用账户的密码。

- 添加：先写密钥，再插入行；插入失败则删除刚写的密钥（`catmail-core` 的 `Storage::insert_with_secret`）。
  和 blob 是同一条规则：行引用的东西先存好，再写行（`catmail-core/src/engine/storage.rs`）。
- 删除：`DELETE … RETURNING secret_key` 原子地删行并取得键，再删密钥。删除账户时在同一个事务里先读出
  它的身份自己的键（身份随账户级联删除），再一并删掉。

任何能看到这一行的人也一定能看到它的密钥，所以并发删除不会留下孤立凭据。唯一的残留窗口是进程恰好
在"写密钥"与"插入行"之间崩溃，只会留下一条无害的孤立条目。

待办：无 Secret Service 的环境（无界面的服务器）读 systemd 的 credentials，见 [MCP / 无界面服务器的密码](mcp.md#无界面服务器的密码)。
