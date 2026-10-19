# cat-mail 架构

跨平台（Linux → Windows / macOS）邮件客户端。核心是一个与 UI 无关的邮件引擎，对外只暴露
`MailClient` 门面；CLI 与 GUI 是平级的两个前端。

本文只描述架构设计。项目范围、里程碑与进度见 [README](../README.md)。

## 分层

自上而下，上层只依赖下层：

| 层 | crate | 内容 |
|---|---|---|
| Frontends | `catmail-cli`、`catmail-gui` | 命令行（clap）；图形界面（gpui-kit） |
| Facade | `catmail-core` | `MailClient`：async 方法 + 事件订阅，前端唯一依赖 |
| Services / Engine | `catmail-core` | Account / Folder / Message / Draft / Send / Attachment 服务；每账户一个 SyncWorker actor、OpQueue、Outbox；运行时桥接、凭据存储 |
| Storage | `catmail-store` | SQLite schema 与迁移、事务化 Repository、FTS5、内容寻址 `BlobStore`；全部同步 API |
| Protocol | `catmail-proto` | IMAP（imap-next）、SMTP、MIME 解析与构建 |
| Domain | `catmail-types` | `Account`、`Folder`、`MessageSummary`、`Flags`、`Event`、`SyncReport`、ID 类型；纯数据，无 IO |

Storage 与 Protocol 是同一层的两个并列模块，彼此不依赖。依赖方向：
`types ← store, proto ← core ← cli / gui`。前端不得直接依赖 `store` / `proto`，需要的类型由
`catmail-core` 重新导出。`catmail-testkit`（假 IMAP 服务器）不在这条链上，只作为
dev-dependency 使用。

## 关键决策

### 每个数据目录一个引擎

`MailClient::open` 对 `<data>/lock` 加独占文件锁（`std::fs::File::try_lock`），整个引擎生命
周期内持有；已被占用时返回 `Error::AlreadyRunning`。同一个数据目录因此只有一个引擎在跑
SyncWorker、消费 Outbox、发事件，不需要跨进程的任务领取、租约或变更通知。

代价：GUI 运行时，CLI 不能打开同一个数据目录。之后让其他前端通过 IPC 连接正在运行的引擎，
而不是各自打开数据库。

### 运行时桥接

GPUI 用自己的 executor，不是 tokio；而 IMAP/SMTP 栈依赖 tokio reactor。`MailClient` 持有
（GUI：`MailClient::builder(cfg).open()` 自建）或借用（CLI：`.runtime(Handle::current())`）
一个 tokio runtime。每个公开 async 方法内部把工作 `spawn` / `spawn_blocking` 到该 runtime
上再 await `JoinHandle`，因此返回的 future 可以被任意 executor poll。
见 `crates/catmail-core/src/runtime.rs`，测试 `owned_runtime_is_usable_from_a_foreign_executor`。

- `Runtime` 与 `Engine` 分开放在两个 `Arc` 里，后台任务只持有 `Engine`，避免在 runtime
  自己的线程上 drop 最后一个 runtime 引用。
- 自建 runtime 在 drop 时 `shutdown_background()`，不等待任务；状态都已落 SQLite。
- 后台任务 panic 转换为 `Error::Task`，不让 GUI 进程跟着崩。

### 事务与事件

所有数据库访问都经过 `Store::read` / `Store::write`：闭包拿到一个 `Db`，其上各个 repository
共用同一个事务。`write` 是 `IMMEDIATE` 事务，闭包返回 `Ok` 才提交。

事件（`tokio::sync::broadcast<Event>`）的规则：

1. **先提交，再发事件**，绝不在 `write` 闭包内发事件，否则订阅者可能读到尚未提交的状态。
2. 事件在执行变更的后台任务内部发出，调用方中途 drop future 也不会丢事件。
3. 事件只携带 ID（"某账户的某文件夹变了"），前端收到后重新查询；`RecvError::Lagged` 时整体
   刷新即可，不会丢状态。

### 离线优先 + 操作队列

用户操作（标记、移动、删除）在**同一个 `write` 事务**里完成两件事：修改本地数据（乐观更新）、
插入 `pending_ops` 记录；提交后再发事件。这样不会出现"界面已改，但服务器永远收不到操作"。

`pending_ops` 每条记录绑定 `folder_id` + `uidvalidity`，UID 只在这个代际内有意义。回放规则：

- 按文件夹分组，`SELECT` 后确认服务器的 UIDVALIDITY 与记录一致才执行；不一致则标记为
  `conflict`，进入重新对账，绝不在新代际里按旧 UID 操作。
- 按 `id` 顺序回放。永久失败时的回滚只撤销这条操作自己的改动，且仅当之后没有其他操作改过
  同一封邮件时才回滚，避免较早的失败覆盖用户后来的修改。

草稿的远端副本同理：`drafts.remote_folder_id` / `remote_uidvalidity` / `remote_uid` 三者
同时设置、同时清空。删除文件夹或 UIDVALIDITY 变化时由 repository 一起清空；表上的 CHECK
约束保证单纯的 `ON DELETE SET NULL` 会失败，而不是留下一个悬空的 UID。

### 同步

目标形态是每账户一个 actor：1 条 IDLE 连接 + 1~2 条工作连接，通过 mpsc 接收命令（M4）。
M1 先实现按需的只读同步 `MailClient::sync_account`：一条连接，逐个文件夹 EXAMINE（只读，
不改 `\Recent`、不会误改服务器状态）。同一账户的同步由每账户一把锁串行化；删除账户时先
发取消信号并等锁释放，再删除数据，正在进行的同步立即返回 `Error::Cancelled`，不用等网络
超时。见 `crates/catmail-core/src/engine.rs`、`sync.rs`。

1. `LIST "" "*"` 对账文件夹：新增、更新属性、删除服务器上已不存在的文件夹（连同邮件）。
   角色依次取：名为 INBOX → SPECIAL-USE 属性 → 常见名称（`Sent Items`、`已发送`、`垃圾邮件`
   等），按名称猜测只用于没有任何文件夹通过 SPECIAL-USE 声明的角色，且每个角色只猜一个。
   `\Noselect` 的层级节点也保存（`selectable = 0`），但不同步。可选中的邮箱变成层级节点时
   （按 RFC 3501，删除有子文件夹的邮箱会清空它并保留名字为 `\Noselect`），按"这一代 UID
   结束"处理：删除邮件、清空 UIDVALIDITY 与游标、离线操作标为 `conflict`，只保留这一行。
2. 对每个可选中的文件夹 EXAMINE（服务器支持时带 `CONDSTORE`），比对 UIDVALIDITY：
   - 变化（或第一次同步）：在同一个事务里删除该文件夹的所有邮件、写入新的 UIDVALIDITY、
     清空游标，把旧代际的 `pending_ops` 标为 `conflict`，并清掉按旧 UID 指向它的草稿远端副本。
   - 一致：M4 起在这里回放该文件夹的 `pending_ops`。
3. 新邮件与已删除邮件：
   - 游标里的 UIDNEXT 与服务器相同、本地条数等于 EXISTS：既没有新邮件也没有删除，不发 SEARCH。
   - 否则先 `UID SEARCH UID <游标>:*` 只取游标之后的 UID；若"本地条数 + 新邮件数 = EXISTS"，
     说明没有删除，大文件夹的完整 UID 列表不必传输。
   - 仍对不上（有删除，或没有游标）时才 `UID SEARCH ALL`，与本地 UID 列表（两者都有序）求差集。
4. 已有邮件的 flags：`UID FETCH 1:<本地最大 UID> (FLAGS)`，有 CONDSTORE 时加
   `(CHANGEDSINCE <游标>)`，HIGHESTMODSEQ 没变则整个跳过；没有 CONDSTORE（或文件夹报告
   NOMODSEQ）时每次全量拉 flags。
5. 拉取新邮件：从新到旧，每批 200 封
   `UID FETCH (UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (…)])`，
   每批一个事务，提交后发 `MessagesChanged`，界面先看到最新的邮件。
6. 文件夹全部完成后才写游标（UIDNEXT、HIGHESTMODSEQ，取自 EXAMINE 时刻）。如果某批拿回的
   邮件比请求的少（SEARCH 之后被删除，或响应无法解析），不写游标，下次完整求差集。

**游标与断点续传。** M0 的设想是"每批结果与游标在同一个事务里提交"，M1 改为"游标只代表
一次完整的同步"：从新到旧拉取时 UIDNEXT 无法按批推进，而已经入库的邮件行本身就是进度——
中断后下一次同步重新求差集，只会拉缺的那些（`upsert` 遇到已有 UID 只刷新 flags，重复的批次
不会产生重复行）。HIGHESTMODSEQ 用的是 EXAMINE 时刻的值，之后发生的变化 modseq 更大，下次
`CHANGEDSINCE` 一定能拿到。

认证失败、服务器缺少设置要求的能力（如 STARTTLS）在 core 里转换成 `Error::AuthFailed` /
`Error::Unsupported`，错误信息里带给用户的建议，前端直接显示即可。

**容错。** 服务器对某个文件夹回 NO/BAD（例如 LIST 之后被删除）只记在该文件夹的报告里，其余
文件夹照常同步；断线、超时、BYE 等让整个同步失败，已提交的进度保留。重复或乱序的 FETCH 响应
按 UID 去重；不带 UID 的主动 FETCH（其他客户端改了 flags）和不在请求集合里的 UID 都忽略；
同一封邮件的属性分在几条 FETCH 响应里时按 UID 合并（RFC 9051 允许）。无法解析的行由连接层
计数，**默认严格**：`Connection::execute` 只要丢弃过一行就返回 `Incomplete`，因为 LIST、
SEARCH、EXAMINE、flags 这类结果缺一行就会被当成文件夹被删、邮件被删（EXISTS 当成 0）、
flags 变化被 HIGHESTMODSEQ 跳过（该文件夹失败，LIST 则整个同步失败，本地状态不动）。新命令
默认就是安全的。EXAMINE 缺少 UIDVALIDITY 或 EXISTS 同样只让该文件夹失败。只有拉邮件头显式
使用 `execute_lossy`：丢失的邮件只是不出现在结果里，那一批算作不完整、不推进游标，下次
再拉。每次网络读写超时 60 秒。

待办（M4）：有未回放的 `pending_ops` 时，flags 同步不能覆盖本地的乐观修改。

IMAP 库选 `imap-next`（sans-IO，基于 `imap-codec`），启用 `starttls` 与
`ext_condstore_qresync`，不启用它自带的 `stream`：`catmail-proto` 自己驱动 sans-IO 状态机，
底层可以是任意 `AsyncRead + AsyncWrite`，便于 STARTTLS 升级和测试。注意：
`imap-codec 2.0.0-alpha.9` 未开启 `nom/alloc`，`catmail-proto` 里显式依赖 `nom` 补上（见其
`Cargo.toml` 注释），上游修复后可移除。

**文件夹名。** 不支持 UTF8=ACCEPT 的服务器用修改版 UTF-7 传输非 ASCII 名称（`&XfJT0ZAB-` =
`已发送`）。`folders.remote_name` 原样保存服务器发来的名称，命令里只用它；`name` 是解码后的
显示名，解码失败时等于原名。

**邮件头。** 只拉 `HEADER.FIELDS (DATE FROM TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES)`，
用 `mail-parser`（`full_encoding`）解析，RFC 2047 编码字和 GBK、GB2312、Big5 等旧字符集都转成
UTF-8。没有（或无法解析）Date 头时用 INTERNALDATE 作为列表的排序时间。`has_attachments` 先由
BODYSTRUCTURE 粗略判断（`MimeNode::has_attachments`，与按部分下载共用同一个 BODYSTRUCTURE
转换），下载正文后按完整 MIME 树重算（见下一节）。

### 正文与附件

正文按需下载（`MailClient::download_message`），不在同步时预取。按 RFC822.SIZE 分两条路径：

- **≤ 2 MiB：整封下载。** `UID FETCH <uid> (BODY.PEEK[])`，原文存入 BlobStore
  （`message_bodies.raw_blob_hash`），用 mail-parser 解析，附件随即解码入库。绝大多数邮件
  走这条路：完整原文最不依赖服务器 BODYSTRUCTURE 的正确性，也留下了 `.eml`（转发、
  "查看原文"、以后换解析器重建）。
- **> 2 MiB：按部分下载。** 先取 BODYSTRUCTURE，只取会作为正文显示的部分（单个不超过
  2 MiB；"会显示"由 mail-parser 在不含任何数据的替身邮件上判定，与显示时同一套规则，见
  下文），附件只记录 section、
  传输编码和估算大小，第一次打开时再 `BODY.PEEK[<section>]` 下载、解码、入库
  （`MailClient::attachment_content`）。BODYSTRUCTURE 解析失败（有的服务器会发错），或服务器
  对某个要取的正文部分回 NIL、干脆不回时，退回整封下载：缺了正文的结果一旦按"已下载"存下，
  就再也不会重取。`raw_message` 遇到尚未下载的大邮件时直接整封下载一次，同时存下正文。

两条路径共用一套提取逻辑：按部分下载时，用 BODYSTRUCTURE 和取回的部分**重建一封结构相同
的替身邮件**（没取的部分是空的 base64 正文），交给 mail-parser 按同样方式解析。于是两条
路径选出的正文部分、字符集与传输编码的解码、RFC 2047 / 2231 文件名的解码完全一致。
BODYSTRUCTURE 里的值来自服务器，写进替身邮件头之前去掉换行、token 只保留合法字符，
不能伪造邮件头；叶子部分即使自称 `multipart` 也按 `application` 写出、去掉 `boundary`
参数，否则它会吞掉后面的部分、打乱 section 编号。替身邮件解析出的部分如果对不上
BODYSTRUCTURE 的 section，一律视为未下载。超过 2 MiB 的文本部分既不下载也不作为附件
列出（已知限制）。IMAP section 编号按 RFC 3501 §6.4.5 从解析结果计算，附加的邮件
（message/rfc822）算一个部分。

正文的组成：`text` 为所有正文文本部分（只有 HTML 时由 HTML 转换）；`html` 只在有真正的
HTML 部分时存在，经 `ammonia` 清洗（去掉脚本、样式表、表单、iframe、事件属性和
`javascript:` 链接；保留表格布局属性和行内 `style`，但可能加载资源的 style（`url(`、
`image-set(`、`@import`、CSS 转义）整个去掉；链接只允许 http、https、mailto、cid，`data:`
只允许出现在 `img src`，判断时像 URL 解析器一样忽略其中的制表符、换行和开头的控制字符）。图片地址（远程、`cid:`）原样保留，是否加载由渲染端决定，
默认应拦截远程图片。"内嵌"（`inline`）指 HTML 用 `cid:` 引用了它（按 RFC 2392 取出完整的
引用值、百分号解码后精确比较，`cid:logo-long` 不会让 `logo` 也算内嵌）；没被引用的图片无论放在
哪里都算附件，`has_attachments` 只统计非内嵌的部分。

**正确性。**

- 只用 EXAMINE 和 `BODY.PEEK`，下载不会把邮件标为已读，也不改服务器上的任何状态。
- 用 UID 之前先 EXAMINE 并比对 UIDVALIDITY；不一致或服务器已没有这封邮件时返回
  `Error::OutOfSync`，不在新代际里按旧 UID 取邮件，交给下一次同步处理。
- 写入时只检查邮件行是否还在：message id 不复用，UIDVALIDITY 变化会删除旧代际的行，所以
  行还在就说明它仍是下载开始时的那个（文件夹, UID）。正文、附件行、摘要、
  `has_attachments` 和全文索引在同一个事务里写入（`Messages::store_body`）；两个并发
  下载同一封邮件时只有一个写入，另一个直接读结果。
- 每账户一份引擎状态（`AccountState`：同步锁、"已删除"标记、空闲连接）。删除账户时先
  置"已删除"、清空连接池，再删除数据；删除成功后这份状态作为墓碑保留（账户 id 不复用），
  之后对它的任何操作立即取消，不必像以前那样"先订阅取消信号、再确认一遍邮件还在"。进行中
  的同步和下载立即返回 `Error::Cancelled`，不用等网络超时；删除后才开始的同步返回
  `AccountNotFound`。删除失败（如 id 不存在）时撤掉标记，以后得到这个 id 的账户不受影响。
  所有按需操作经过同一个入口（`body.rs` 的 `on_server`）：取连接、EXAMINE 比对
  UIDVALIDITY、在取消信号上 select。

**连接。** 按需下载使用每账户最多 2 条空闲连接的小连接池（同步仍用自己的连接，打开邮件
不必等同步结束），空闲超过 5 分钟的连接不再复用。复用的连接已断开时（服务器或 NAT 关掉了
空闲连接）自动换一条**新建**的连接重试一次（不再从池里取，池里其余的连接多半也已断开）；
新连接上的失败直接返回。归还连接时在连接池的锁内检查"已删除"标记，而删除是先置标记
再取这把锁清空连接池，所以不会有连接在账户删除后留在池里。M4 的每账户 actor 会接管这部分。
复用的连接仍然每次 EXAMINE：省掉这一个往返要依赖服务器绝不在选中期间改变 UIDVALIDITY，
而这正是 UIDVALIDITY 检查要防的情况。

### 认证

`catmail_proto::imap::Credentials` 是协议层看到的全部：目前只有
`Password { username, password }`，登录时服务器支持 `AUTH=PLAIN` 就用 `AUTHENTICATE PLAIN`，
否则用 `LOGIN`（`LOGINDISABLED` 时报错）。凭据从哪来由 `catmail-core/src/auth.rs` 按
`AuthKind` 决定（密码账户读 keyring）。加 OAuth2（M6）只需新增 `AuthKind` 和 `Credentials`
的变体（XOAUTH2 / OAUTHBEARER）以及刷新 token 的那一个分支，同步代码不变。

### TLS

`rustls`，crypto provider 用 `ring`（不用默认的 aws-lc-rs，Windows CI 不需要 cmake / NASM）。
证书校验用 `rustls-platform-verifier`，即操作系统的信任库与校验逻辑，企业内部 CA 与浏览器
表现一致。`MailClientBuilder::tls` 可以替换，例如 `TlsConfig::with_roots_only(<DER 证书>)`
只信任自签名证书；接口只收字节，前端不需要依赖 rustls。

- `tls`：从第一个字节开始 TLS。
- `starttls`：服务器不支持 STARTTLS 就报错，绝不降级为明文；升级后丢弃明文阶段的一切状态
  （包括可能被注入的、跟在 `OK` 后面的字节）并重新获取 CAPABILITY；PREAUTH 视为不支持。
- `tls` / `plain` 下的 PREAUTH 问候表示已认证，`login` 直接返回。
- `plain`：明文，只用于本地测试服务器。

### 存储

- SQLite（`rusqlite` bundled，三平台行为一致、自带 FTS5），WAL，`foreign_keys=ON`。
  目前单连接 + Mutex；读写竞争出现后拆成 1 写 + 读池。
- 迁移在 `crates/catmail-store/migrations/`，由 `rusqlite_migration`（`user_version`）执行。
  发布之后，迁移文件不可修改，改 schema 一律新增文件。
- 约定：时间为 unix 秒；枚举列为 TEXT 不加 CHECK（新增变体无需重建表）；地址列表为 JSON；
  外部可见的 ID（accounts / folders / messages / drafts）用 `AUTOINCREMENT`，删除后不复用。

**邮件模型。** `messages` 的一行是"某个文件夹里的一封邮件"，即一个 IMAP（mailbox, UID）对，
与服务器的语义一致：

- flags、keywords、MODSEQ 都是这一行自己的状态。同一封邮件在 Inbox 已读、在 Archive 未读，
  就是两行、各自的 flags。
- 移动到别的文件夹得到新的 UID，也就是新的一行。
- 不按 Message-ID 合并不同文件夹里的副本（它不可靠）。正文不需要专门共享：BlobStore 按内容
  存储，相同的原始邮件在磁盘上只有一份。
- 以后支持 Gmail 时，可加可空的 `X-GM-MSGID` / `EMAILID`（RFC 8474）列来关联副本，但它只用于
  分组，不能替代每一行自己的状态。

**计数。** `folders.unread_count` / `total_count` 由 `messages` 上的触发器维护（插入、删除、
`\Seen` 变化各 O(1)），不论变化来自同步、本地操作还是级联删除，写入方都不用记得刷新。

**跨账户约束。** 引用其他账户内数据的行，用 `(id, account_id)` 复合外键约束，数据库层面就
不可能把账户 A 的邮件挂到账户 B 的文件夹下（`messages`、`pending_ops` → `folders`）。
`ON DELETE SET NULL` 的关联（如 `drafts.reply_to_message_id`）没法用复合外键，因为
SET NULL 会连 `account_id` 一起置空，这类关联由 repository 校验。

**全文搜索。** `messages_fts` 为 contentless FTS5（`contentless_delete=1`），rowid =
`messages.id`，列为主题、发件人（姓名和地址）、收件人与抄送、正文。`Messages` repository
在插入邮件时写入（此时正文为空），正文下载后在同一事务里删除并重写这一行；删除邮件（含
级联删除）由触发器删除索引。所以邮件头总能搜到，正文只有下载过的才能搜到。

unicode61 分词器会把连续的中日韩文字当成一个词，trigram 又不支持两个字的查询，所以在
Rust 侧预先切分（`crates/catmail-store/src/fts.rs`）：一段连续的 CJK 文字写成它的相邻双字，
后面再跟单字，如 `季度报告` → `季度 度报 报告 季 度 报 告`。查询时一个字匹配单字，两个字
匹配双字，三个字以上变成相邻双字组成的短语（`"季度 度报 报告"`）；同一段的双字位置连续，
所以短语恰好匹配原来的子串，不会跨段误配。其他文字交给 unicode61（忽略大小写和变音
符号，按整词匹配）。全角 ASCII 折叠成半角。查询语法（`SearchQuery`）：空格分隔的词或
`"短语"` 都要匹配，可加 `from:`、`to:`（含抄送）、`subject:` 前缀；每个词都放进 FTS5
字符串里，用户输入不会被当成 FTS5 语法。混合文字的词（`iPhone手机`）拆成几段分别匹配。
结果按日期倒序。

### 附件 / BlobStore

内容寻址：`<data>/blobs/ab/cdef…`（SHA-256）。写入先进 `blobs/tmp`、fsync 后 rename，读者
看不到半成品；相同内容只存一份。rename 从不覆盖已有文件（`persist_noclobber`）：同一内容
可能被两次下载同时写入，而 Windows 上覆盖一个正被别人覆盖或打开的文件会失败；目标已存在就
说明别人已经存好了，刷新 mtime 即可。DB 只存 hash，没有 blob 表。收信附件见上文"正文与附件"
（imap-next 会把整条响应读进内存，所以附件是取回后再写入，不是流式落盘）；写信附件在添加
时即复制入库，原文件移动/删除不影响草稿。

**GC 约定。** 从 `put` 返回到引用它的数据库行提交之间有一个窗口，而且 `put` 可能
返回一个早已存在的 hash。因此只删除"没有被任何 `*_blob_hash` 列引用，**并且** mtime 早于
宽限期"的文件，删除前再检查一次 mtime。宽限期要远大于任何写事务的时长。`put` 遇到已存在的
blob 时会刷新它的 mtime，让宽限期重新计时。

实现（`BlobStore::collect_garbage`，入口 `MailClient::collect_garbage` / `catmail gc`）：先在
一个读事务里取出所有引用（`Db::blob_references`；有测试枚举所有 `*_blob_hash` 列，漏加会失败），再
遍历目录找出未引用且足够旧的候选，逐个在写锁下重查 mtime 后删除。`put` 刷新或创建 blob 时
持有同一把锁的读锁，所以"GC 判定为旧"与"`put` 刚刚刷新"不会交错。宽限期 1 小时；`tmp`
里中断写入留下的旧文件一并清理。删除账户、UIDVALIDITY 变化等删除邮件之后，它们的 blob
由下一次 GC 回收。

### 凭据

密码不进数据库。`SecretStore` trait，默认 `KeyringSecrets`（`keyring` 4：Linux Secret
Service / macOS Keychain / Windows Credential Manager），service 为 `cat-mail`，键为
`account:{email}:{随机 uuid}`，存在 `accounts.secret_key` 列。随机键保证不同数据目录的凭据
互不覆盖。

顺序是正确性的关键：
- 添加：先写密钥，再插入行；插入失败则删除刚写的密钥。
- 删除：`DELETE … RETURNING secret_key` 原子地删行并取得键，再删密钥。

任何能看到这一行的人也一定能看到它的密钥，所以并发删除不会留下孤立凭据。唯一的残留窗口是
进程恰好在"写密钥"与"插入行"之间崩溃，只会留下一条无害的孤立条目。待办：无 Secret Service
的环境（纯 TTY）提供加密文件降级。

### 发信

Outbox 的每一行是一份自包含的发送快照，重试时不再读取草稿：MIME 原文（blob），加上单独存储
的 SMTP 信封 `envelope_from` 与 `recipients_json`（所有 RCPT TO，包括密送；密送收件人不在
MIME 头里）。

状态机：

| 状态 | 含义 | 之后 |
|---|---|---|
| `queued` | 等待发送（`next_attempt_at`） | → `sending` |
| `sending` | SMTP 事务进行中 | 成功 → `accepted`；DATA 完成前可重试的失败 → `queued`；5xx → `failed`；DATA 结束后连接断开 → `unknown` |
| `accepted` | 服务器已接受（DATA 后返回 250），待保存到 Sent | 只重试 APPEND，**绝不重发 SMTP** → `done` |
| `unknown` | 发送结果未知 | **不自动重试**（重发可能导致重复投递），由用户决定 |
| `done` | 已发送，且需要时已保存到 Sent | 删除草稿，发 `MessageSent` 事件 |
| `failed` | 被永久拒绝，原因在 `last_error` | 由用户处理 |

### GUI

`gpui-kit`（longbridge）：虚拟列表 / Table / 编辑器 / 原生 HTML 渲染，无 WebView。
它构建在 `gpui-pre-*` 快照上并精确 pin 版本，`gpui` 依赖需与其保持一致，否则会链接两份 GPUI。

HTML 邮件是主要风险：gpui-kit 的 HTML 渲染定位为简单 HTML，需要尽早用真实邮件验证表格、
`cid:` 内嵌图片和中文排版。默认优先 `text/plain`，HTML 经 `ammonia` 清洗后交给 gpui-kit
渲染，默认拦截远程图片，提供"在浏览器打开"兜底；效果不够时再评估嵌入 `wry` 或专用转换器。
清洗后的 HTML 仍保留远程图片地址和行内样式，浏览器兜底打开时要加 CSP（`default-src 'none';
img-src cid: data:; style-src 'unsafe-inline'`），否则浏览器会替用户加载远程内容。

## 测试环境

- **假 IMAP 服务器**（`crates/catmail-testkit`，只作为 dev-dependency）：进程内、可修改邮箱
  内容，支持明文 / TLS / STARTTLS，可注入故障：断线（第 n 次拉邮件头时）、拉取时挂起、重复
  或倒序的 FETCH 响应、不带 UID 的主动 FETCH、没有 CONDSTORE、UIDVALIDITY 改变、STARTTLS 之后
  的明文注入、拒绝登录、拉正文时断线、挂起或按闸门放行、section 回 NIL、关闭当前所有连接。BODYSTRUCTURE 和各 section 由每封邮件的
  原文推导（mail-parser），所以测试邮件就是普通的 `.eml`。直接按字节写响应，故障可以精确
  到线上格式。三个平台的 CI 都跑。
- **真实服务器**：`testenv/dovecot/run.sh` 以当前用户在临时目录里启动 Dovecot（来自 flake 的
  `imap` 开发环境，版本随 `flake.lock` 固定），预置 `testenv/fixtures/` 里的邮件，包括中文
  文件夹、层级文件夹、各种编码、HTML 加内嵌图片与 RFC 2231 文件名；测试还会投递一封超过
  2 MiB 的邮件，走按部分下载的路径。`crates/catmail-core/tests/dovecot.rs` 在设置了
  `CATMAIL_TEST_IMAP` 时运行，CI 里有单独的 Linux job。

覆盖不到、留给后续的：进程在同步中途被杀（目前用"断线后重新同步"近似，事务边界相同）；
M3 的 SMTP 确认丢失、保存到 Sent 失败。

## 数据目录

`Config::platform_default()`（`directories`）：Linux `~/.local/share/catmail`，
macOS `~/Library/Application Support/dev.CatMail.CatMail`，Windows `%APPDATA%\CatMail\CatMail\data`。
CLI 可用 `--data-dir` 或 `CATMAIL_DATA_DIR` 覆盖。目录内：`catmail.db`、`blobs/`、`lock`。
