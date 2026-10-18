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
| Domain | `catmail-types` | `Account`、`Folder`、`MessageSummary`、`Flags`、`Event`、ID 类型；纯数据，无 IO |

Storage 与 Protocol 是同一层的两个并列模块，彼此不依赖。依赖方向：
`types ← store, proto ← core ← cli / gui`。前端不得直接依赖 `store` / `proto`，需要的类型由
`catmail-core` 重新导出。

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
同时设置、同时清空。

### 同步

每账户一个 actor：1 条 IDLE 连接 + 1~2 条工作连接，通过 mpsc 接收命令。删除账户时先停止
该账户的 actor，再删除数据。

1. `LIST` 对账文件夹，SPECIAL-USE 识别角色。
2. 对每个文件夹 `SELECT`（CONDSTORE/QRESYNC），先比对 UIDVALIDITY：
   - 变化：在同一个事务里删除该文件夹的所有邮件、写入新的 UIDVALIDITY，并把该文件夹待回放
     的 `pending_ops` 标为 `conflict`。
   - 一致：回放该文件夹的 `pending_ops`。
3. 拉取变化：`UID FETCH uidnext:*` 拉 ENVELOPE / FLAGS / BODYSTRUCTURE / INTERNALDATE；
   `CHANGEDSINCE` 拉 flags 变化；`VANISHED`（或 `UID SEARCH ALL` 差集）处理删除。
   每批结果与该文件夹的同步游标（uidnext、highest_modseq）在同一个事务里提交。
4. 正文按需 + 预取最近 N 封；附件仅在打开时拉取。

IMAP 库选 `imap-next`（sans-IO，基于 `imap-codec`），启用 `starttls` 与
`ext_condstore_qresync`。注意：`imap-codec 2.0.0-alpha.9` 未开启 `nom/alloc`，
`catmail-proto` 里显式依赖 `nom` 补上（见其 `Cargo.toml` 注释），上游修复后可移除。

认证是可替换的接口（目前只有密码实现），加 OAuth2 不需要改同步代码。

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

**跨账户约束。** 引用其他账户内数据的行，用 `(id, account_id)` 复合外键约束，数据库层面就
不可能把账户 A 的邮件挂到账户 B 的文件夹下（`messages`、`pending_ops` → `folders`）。
`ON DELETE SET NULL` 的关联（如 `drafts.reply_to_message_id`）没法用复合外键，因为
SET NULL 会连 `account_id` 一起置空，这类关联由 repository 校验。

**全文搜索。** `messages_fts` 为 contentless FTS5（`contentless_delete=1`），rowid =
`messages.id`，由触发器随 `messages` 删除（含级联删除）。unicode61 分词器会把连续的中文
当成一个词，trigram 又不支持两个字的查询，所以在 Rust 侧对中日韩文字预先切分（单字 +
相邻双字），写入索引和查询时用同一套切分。

### 附件 / BlobStore

内容寻址：`<data>/blobs/ab/cdef…`（SHA-256）。写入先进 `blobs/tmp`、fsync 后 rename，读者
看不到半成品；相同内容只存一份。DB 只存 hash，没有 blob 表。收信附件用
`BODY.PEEK[section]` 流式落盘；写信附件在添加时即复制入库，原文件移动/删除不影响草稿。

**GC 约定。** 从 `put` 返回到引用它的数据库行提交之间有一个窗口，而且 `put` 可能
返回一个早已存在的 hash。因此只删除"没有被任何 `*_blob_hash` 列引用，**并且** mtime 早于
宽限期"的文件，删除前再检查一次 mtime。宽限期要远大于任何写事务的时长。`put` 遇到已存在的
blob 时会刷新它的 mtime，让宽限期重新计时。

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

## 数据目录

`Config::platform_default()`（`directories`）：Linux `~/.local/share/catmail`，
macOS `~/Library/Application Support/dev.CatMail.CatMail`，Windows `%APPDATA%\CatMail\CatMail\data`。
CLI 可用 `--data-dir` 或 `CATMAIL_DATA_DIR` 覆盖。目录内：`catmail.db`、`blobs/`、`lock`。
