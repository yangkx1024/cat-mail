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
- 迁移在 `crates/catmail-store/migrations/`，由 `rusqlite_migration`（`user_version`）执行。发布
  之后迁移文件不可修改，改 schema 一律新增文件。
- 约定：时间为 unix 秒；枚举列为 TEXT 不加 CHECK（新增变体无需重建表）；地址列表为 JSON；外部可见
  的 ID（accounts / folders / messages / drafts）用 `AUTOINCREMENT`，删除后不复用。

## 邮件行

一行是服务器上的一封邮件：它的位置（`kind = 'location'` 的文件夹）和那里的 UID，见
[架构](../architecture.md#一行是服务器上的一封邮件uid-绑定-uidvalidity)。Gmail 上一行是一个 `X-GM-MSGID`
（`gm_msgid`，账户内唯一），其余邮箱是标签（`message_labels`），见[位置与标签](labels.md#存储)；同步还没有
按这个模型写入。补充：

- 移动得到新的 UID：服务器给出 COPYUID 时同一行改为新的（文件夹, UID），否则旧行删除、同步时成为
  新的一行。本地移动过来、服务器还没移动的行 `uid` 为空。
- 普通账户上不同文件夹里的副本不关联：`EMAILID`（RFC 8474）相同的副本 flags 仍各自独立，只能用于分组。
  会话分组（`X-GM-THRID` / `THREADID`）等决定怎么做时再加列，现在不预留。
- 索引只为现有查询而建：每条索引都让同步的每次插入、删除多一棵 B 树要改。按账户列出邮件（统一收件箱）
  等查询出现时再加对应索引。

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
rowid 即 `id`，不必再列一次）：先取游标那个日期里 `id` 更小的（`LIST_SAME_DATE`，定位到
`folder_id, list_date, rowid`），不够一页再取更早的日期（`LIST_OLDER`）。一个行值条件
`(list_date, id) < (…)` 看似等价，但 SQLite 只用 `list_date` 定位，要逐行跨过同一日期里 `id` 更大的
所有行：几万封同一时间（或都没有日期）的邮件里，越往后翻每页越慢，比 OFFSET 还慢。没有游标时绑定
`(i64::MAX, i64::MAX)`。有测试比较翻到一组同日期邮件末尾和开头时 `list` 走的 VM 步数。

搜索先由 FTS 取出全部匹配再排序，用不上索引，游标只是过滤，所以一个行值条件（`AFTER`）就够了。

## 全文搜索

`messages_fts` 为 contentless FTS5（`contentless_delete=1`），rowid = `messages.id`，列为主题、
发件人（姓名和地址）、收件人与抄送、正文。插入邮件时写入（正文为空），正文下载后在同一事务里删除
并重写这一行；删除邮件（含级联删除）由触发器删除索引。所以邮件头总能搜到，正文只有下载过的才能搜到。

**中日韩分词。** unicode61 会把连续的 CJK 文字当成一个词，trigram 又不支持两个字的查询，所以在 Rust
侧预先切分（`crates/catmail-store/src/fts.rs`）：一段连续的 CJK 文字写成相邻双字，后面再跟单字，
如 `季度报告` → `季度 度报 报告 季 度 报 告`。查询时一个字匹配单字，两个字匹配双字，三个字以上变成
相邻双字组成的短语（`"季度 度报 报告"`）；同一段的双字位置连续，所以短语恰好匹配原来的子串，不会
跨段误配。其他文字交给 unicode61（忽略大小写和变音符号，按整词匹配）；全角 ASCII 折叠成半角；混合
文字的词（`iPhone手机`）拆成几段分别匹配。

**查询语法**（`SearchQuery`）：空格分隔的词或 `"短语"` 都要匹配，可加 `from:`、`to:`（含抄送）、
`subject:` 前缀。每个词都放进 FTS5 字符串里，用户输入不会被当成 FTS5 语法。结果按日期倒序。

**Gmail 的标签只出一条结果。** 有 `\All` 邮箱的账户（Gmail）上文件夹是标签，一封邮件在 INBOX、
"所有邮件"、"重要"里各有一行，搜索结果按 (账户, Message-ID) 只取一行（窗口函数 `row_number()`，在
游标和 `LIMIT` 之前取，翻页稳定）。取哪一行按 `SEARCH_PICK`：本地刚移过来、服务器还不知道的那行（用户
最后放的位置）优先；其次是邮件真正所在的文件夹（INBOX、已发送、草稿、归档、用户标签、垃圾邮件、已删除），
最后才是只汇集其他文件夹的"已加星标"和"所有邮件"（不是 Gmail 的服务器上，`\All` 可能也包含已删除
邮件，那时应显示在已删除里）。没有 Message-ID 的认不出是同一封，照常各出一条。只影响搜索结果的显示，
行本身仍按文件夹各自存储、各有 flags；没有 `\All` 的账户上不同文件夹里的副本是独立的邮件，不合并。
这是过渡做法：改为[按标签建模](labels.md)之后邮件只有一行，这一节删除。

## BlobStore

内容寻址：`<data>/blobs/ab/cdef…`（SHA-256），相同内容只存一份。DB 只存 hash，没有 blob 表。

- 写入先进 `blobs/tmp`、fsync 后 rename，读者看不到半成品。
- rename 从不覆盖已有文件（`persist_noclobber`）：同一内容可能被两次下载同时写入，而 Windows 上覆盖
  一个正被别人覆盖或打开的文件会失败。目标已存在说明别人已经存好了，刷新 mtime 即可。

**GC 约定。** 从 `put` 返回到引用它的数据库行提交之间有一个窗口，而且 `put` 可能返回一个早已存在的
hash。因此只删除"没有被任何 `*_blob_hash` 列引用，**并且** mtime 早于宽限期"的文件，删除前再查一次
mtime。宽限期要远大于任何写事务的时长；`put` 遇到已存在的 blob 会刷新 mtime，宽限期重新计时。

**实现**（`BlobStore::collect_garbage`，入口 `MailClient::collect_garbage` / `catmail gc`）：先在一个
读事务里取出所有引用（`Db::blob_references`；有测试枚举所有 `*_blob_hash` 列，漏加会失败），再遍历
目录找出未引用且足够旧的候选，逐个在写锁下重查 mtime 后删除。`put` 刷新或创建 blob 时持有同一把锁
的读锁，"GC 判定为旧"与"`put` 刚刚刷新"不会交错。宽限期 1 小时；`tmp` 里中断写入留下的旧文件一并
清理。删除账户、UIDVALIDITY 变化等删除邮件之后，它们的 blob 由下一次 GC 回收。

**谁来运行。** `catmail gc` 打不开别的前端正占着的数据目录，所以长期运行的前端让引擎定期运行
（`MailClientBuilder::collect_garbage_every`，`gc.rs`）：打开后最多 5 分钟第一次（不与启动时的同步
抢，也赶在只开一会儿的窗口关掉之前），之后按给定间隔。GUI 和 `catmail watch` 每 6 小时一次。

## 凭据

`SecretStore` trait，默认 `KeyringSecrets`（`keyring` 4：Linux Secret Service / macOS Keychain /
Windows Credential Manager），service 为 `cat-mail`，键为 `account:{email}:{随机 uuid}`，存在
`accounts.secret_key` 列。随机键保证不同数据目录的凭据互不覆盖。之后添加的发件身份（`identities`，见
[发信](sending.md#发件身份)）各有自己的密码，键为 `identity:{email}:{随机 uuid}`，规则相同；随账户建出的
那个身份没有自己的条目（`secret_key` 为空），用账户的密码。

- 添加：先写密钥，再插入行；插入失败则删除刚写的密钥。
- 删除：`DELETE … RETURNING secret_key` 原子地删行并取得键，再删密钥。删除账户时在同一个事务里先读出
  它的身份自己的键（身份随账户级联删除），再一并删掉。

任何能看到这一行的人也一定能看到它的密钥，所以并发删除不会留下孤立凭据。唯一的残留窗口是进程恰好
在"写密钥"与"插入行"之间崩溃，只会留下一条无害的孤立条目。

待办：无 Secret Service 的环境（纯 TTY）提供加密文件降级。
