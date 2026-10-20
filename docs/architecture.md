# cat-mail 架构

跨平台（Linux → Windows / macOS）邮件客户端。核心是一个与 UI 无关的邮件引擎，对外只暴露
`MailClient` 门面；CLI 与 GUI 是平级的两个前端。

本文只讲整体结构和贯穿各子系统的设计决定，实现细节见[子系统文档](#子系统)。项目范围、里程碑
与进度见 [README](../README.md)。

## 分层

| 层 | crate | 内容 |
|---|---|---|
| Frontends | `catmail-cli`、`catmail-gui` | 命令行（clap）；图形界面（gpui-kit） |
| Facade | `catmail-core` | `MailClient`：async 方法 + 事件订阅，前端唯一依赖 |
| Services / Engine | `catmail-core` | 同步、后台同步（IDLE）、操作队列回放、按需下载、Outbox；每账户状态；运行时桥接；凭据 |
| Storage | `catmail-store` | SQLite schema 与迁移、事务化 Repository、FTS5、内容寻址 `BlobStore`；全部同步 API |
| Protocol | `catmail-proto` | IMAP（imap-next）、SMTP（自己实现）、MIME 解析（mail-parser）与构建（mail-builder）、HTML 清洗 |
| Domain | `catmail-types` | `Account`、`Folder`、`MessageSummary`、`Flags`、`Event`、ID 类型；纯数据，无 IO |

依赖方向 `types ← store, proto ← core ← cli / gui`：store 与 proto 并列、互不依赖；前端不直接
依赖 store / proto，需要的类型由 `catmail-core` 重新导出。`catmail-testkit`（假 IMAP / SMTP
服务器）只作为 dev-dependency。

## 关键设计

### 每个数据目录一个引擎

`MailClient::open` 对 `<data>/lock` 加独占文件锁并持有到引擎关闭，已被占用时返回
`Error::AlreadyRunning`。同一个数据目录只有一个引擎在同步、发信、发事件，因此不需要跨进程的
任务领取、租约或变更通知。

代价是 GUI 运行时 CLI 不能打开同一个数据目录。以后让其他前端通过 IPC 连接正在运行的引擎，
而不是各自打开数据库。

### 运行时桥接

GPUI 有自己的 executor，不是 tokio；IMAP / SMTP 栈却依赖 tokio reactor。`MailClient` 持有
（GUI：`MailClient::builder(cfg).open()` 自建）或借用（CLI：`.runtime(Handle::current())`）
一个 tokio runtime，每个公开方法把工作 `spawn` / `spawn_blocking` 到该 runtime 上再 await
`JoinHandle`，返回的 future 因此可以被任意 executor poll（`catmail-core/src/runtime.rs`）。

- `Runtime` 与 `Engine` 分放在两个 `Arc` 里，后台任务只持有 `Engine`，不会在 runtime 自己的
  线程上 drop 最后一个 runtime 引用。
- 自建 runtime drop 时 `shutdown_background()`，不等待任务：状态都已落 SQLite。
- 后台任务 panic 转成 `Error::Task`，GUI 进程不跟着崩。

### 事务与事件

数据库访问只经过 `Store::read` / `Store::write`：闭包里的各个 repository 共用同一个事务；
`write` 是 `IMMEDIATE` 事务，闭包返回 `Ok` 才提交。必须原子的东西（本地改动与它的
`pending_ops` 行、同步来的邮件与文件夹游标）放在同一个闭包里。

事件（`tokio::sync::broadcast<Event>`）的规则：

1. **先提交，再发事件**，绝不在 `write` 闭包内发，否则订阅者可能读到尚未提交的状态。
2. 由执行变更的后台任务发出，调用方中途 drop future 也不会丢事件。
3. 只携带 ID（"某账户的某文件夹变了"），前端收到后重新查询；`RecvError::Lagged` 时整体刷新，
   不会丢状态。

### 邮件按文件夹存储，UID 绑定 UIDVALIDITY

`messages` 的一行是一个 IMAP（文件夹, UID）对，与服务器的语义一致：flags、keywords、MODSEQ
都是这一行自己的状态，同一封信在 Inbox 已读、在 Archive 未读就是两行。不按 Message-ID 合并
不同文件夹里的副本（它不可靠）；相同的原文由内容寻址的 BlobStore 自然只存一份。

UID 只在一个 UIDVALIDITY 内有意义。凡是持久化的、指向远端 UID 的东西（操作队列、草稿的远端
副本）都同时记下文件夹和它的 UIDVALIDITY，使用前核对。UIDVALIDITY 变化时这个文件夹的旧代际
整体作废：本地邮件删除重拉，指向它的操作标为冲突，绝不在新代际里按旧 UID 操作。

### 读不改服务器，改动走操作队列

同步和下载正文只用 EXAMINE 与 `BODY.PEEK`，不会把邮件标为已读，也不改服务器上的任何状态。
改动服务器的只有两条路：

- 用户对邮件的操作（标记、移动、删除）：在同一个事务里修改本地数据并写入 `pending_ops`，
  之后在 SELECT 下核对 UIDVALIDITY 再回放。离线照常可用，服务器拒绝时撤销本地改动。
- 用 APPEND 写入新邮件：发信后保存到 Sent；草稿每次修改后上传到 Drafts，它替换掉的旧副本经
  操作队列删除。APPEND 的答复可能丢失，所以每次 APPEND 都能事后按 Message-ID 查到，重试不会
  多出一份。

### 发信绝不在背后重复

发信先把草稿冻结成 outbox 里的一份 MIME 快照，之后每次尝试都发同一份字节。SMTP 发出结束标记
`.` 之前，先用 `write_durable` 提交 `unknown` 状态：在那之前的失败都能安全重试（服务器会丢弃
没结束的事务），从那之后不论断线、超时还是进程被杀都不自动重发，只由用户决定。保存到 Sent
失败只重试 APPEND，不会重发邮件。

### 每账户状态

引擎为每个账户保留一份 `AccountState`：同步锁（同步、操作回放与后台同步的每一步串行）、发送锁
（同一条 outbox 不会被同时处理）、按需下载用的小连接池、唤醒后台同步的信号、"已删除"标记，以及
最近几次连接服务器的结果（`MailClient::account_status`：同步、发信各自是否连续失败、是否登录被拒绝，
变化时发 `AccountStatusChanged`）。连接和登录失败属于账户，不算在某封信或某次操作头上。删除账户时先置标记、清空连接池，进行中
的同步和下载立即返回 `Error::Cancelled`，不用等网络超时，然后才删除数据；这份状态作为墓碑
保留（账户 id 不复用），之后对它的任何操作立即取消。

开启后台同步（GUI、`catmail watch`）时，每个账户还有一个后台任务和它自己的一条连接：定时同步全部
文件夹，其间在 INBOX 上 IDLE（服务器不支持时轮询），本地改动入队后立即回放（见
[同步 / 后台同步](architecture/sync.md#后台同步与-idle)）。它与手动操作共用上面这些锁，而不是做成独占
全部连接、经 mpsc 接收命令的 actor：门面的方法已经按锁串行，按需下载有连接池，都经一个 actor 转发
不增加任何保证，反而让打开邮件排在同步后面。

### 安全边界

- **凭据**：密码只在系统密钥环里，不进 SQLite。添加账户先写密钥再插入行；删除先删行
  （`RETURNING secret_key`）再删密钥，并发删除不会留下孤立凭据。
- **TLS**：rustls + 操作系统的信任库；配置了 STARTTLS 就必须成功，绝不降级为明文。
- **邮件内容不可信**：HTML 入库前经 ammonia 清洗；远程图片默认不加载，GUI 读信不发出任何网络
  请求；服务器给的值（BODYSTRUCTURE、msg-id）写进邮件头之前都要校验。
- **本地数据**：数据目录只有本用户可访问，交给其他程序的文件也放在里面。

## 子系统

| 文档 | 内容 |
|---|---|
| [同步](architecture/sync.md) | 文件夹对账、UIDVALIDITY、增量拉取、游标与断点续传、容错、邮件头 |
| [操作队列](architecture/op-queue.md) | 标记 / 移动 / 删除的本地效果、回放顺序、拒绝时的撤销、无 MOVE 服务器上的 COPY |
| [正文与附件](architecture/bodies.md) | 按需下载的两条路径、正文提取、HTML 清洗、内嵌图片 |
| [草稿与发信](architecture/sending.md) | 草稿与回复、草稿同步到服务器、outbox 状态机、SMTP 客户端、重试、保存到 Sent |
| [IMAP / SMTP 连接](architecture/protocol.md) | imap-next、连接层的严格解析、连接池、认证、TLS |
| [存储](architecture/storage.md) | SQLite 约定、计数、跨账户约束、全文搜索、BlobStore 与 GC、凭据 |
| [GUI](architecture/gui.md) | 视图结构、事件刷新、HTML 渲染与图片策略、已知问题 |
| [测试](architecture/testing.md) | 假 IMAP / SMTP 服务器、真实 Dovecot 与 mailpit、GUI 测试 |

## 数据目录

`Config::platform_default()`（`directories`）：Linux `~/.local/share/catmail`，macOS
`~/Library/Application Support/dev.CatMail.CatMail`，Windows `%APPDATA%\CatMail\CatMail\data`。
CLI 和 GUI 都可用 `--data-dir` 或 `CATMAIL_DATA_DIR` 覆盖。

| 路径 | 内容 |
|---|---|
| `catmail.db` | SQLite：账户、文件夹、邮件头与正文、草稿、队列 |
| `blobs/` | 原文、附件等内容寻址文件 |
| `lock` | 引擎的独占锁 |
| `handoff/` | 前端交给其他程序的文件（如给浏览器的邮件页面） |

目录新建时只有本用户可访问（Unix 上 0700；只设置目录本身，已存在的不改）。`MailClient::open`
拿到锁之后清空 `handoff/`：锁保证没有前端还在往里写，各前端不必各自清理。在打开时而不是关闭时
清，是因为关闭时别的程序可能还在读，macOS 上应用退出也不执行析构。
