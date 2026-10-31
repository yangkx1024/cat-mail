# catmail 架构

跨平台（Linux → Windows / macOS）邮件客户端。核心是一个与 UI 无关的邮件引擎，对外只暴露
`MailClient` 门面；CLI 与 GUI 是平级的两个前端。

本文只讲整体结构和贯穿各子系统的设计决定，实现细节见[子系统文档](#子系统)。项目范围、里程碑
与进度见 [README](../README.md)。

## 分层

| 层 | crate | 内容 |
|---|---|---|
| Frontends | `catmail-cli`、`catmail-gui` | 命令行（clap）；图形界面（gpui-kit） |
| Rendering | `catmail-render` | 读信区的 HTML 引擎（Blitz）：在自己的线程上排版、分块绘制成像素；不依赖 UI 库和邮件类型 |
| Facade | `catmail-core` | `MailClient`：async 方法 + 事件订阅，前端唯一依赖 |
| Services / Engine | `catmail-core` | 同步、后台同步（IDLE）、操作队列回放、按需下载、Outbox；每账户状态；运行时桥接；凭据 |
| Storage | `catmail-store` | SQLite schema 与迁移、事务化 Repository、FTS5、内容寻址 `BlobStore`；全部同步 API |
| Protocol | `catmail-proto` | IMAP（自己的命令编码与连接驱动，imap-proto 解析响应）、SMTP（自己实现）、MIME 解析（mail-parser）与构建（mail-builder）、远程图片的 HTTP 下载（reqwest） |
| HTML | `catmail-html` | 邮件 HTML 的清洗（ammonia，样式表逐记号重写），以及读信区在清洗结果上做的几遍：图片、内嵌图片、深色时的颜色转换；纯函数，无 IO |
| Domain | `catmail-types` | `Account`、`Folder`、`MessageSummary`、`Flags`、`Event`、ID 类型；纯数据，无 IO |

依赖方向 `types ← store, proto ← core ← cli / gui`，`html ← proto, core`：store 与 proto 并列、互不依赖；前端不直接
依赖 store / proto，需要的类型由 `catmail-core` 重新导出。`catmail-testkit`（假 IMAP / SMTP
服务器）只作为 dev-dependency。`catmail-render` 只被 GUI 依赖，它不认识邮件：读信区交给它清洗后的
HTML 和决定哪些图片可以加载的函数。

## 关键设计

### 每个数据目录一个引擎

`MailClient::open` 对 `<data>/lock` 加独占文件锁并持有到引擎关闭，已被占用时返回
`Error::AlreadyRunning`。前端也可以先自己加锁（`Config::lock`）再交给引擎（`MailClient::builder_locked`）：
GUI 这样在打开引擎之前就知道自己是这个数据目录的实例（见[后台运行](architecture/background.md#单实例与唤起)）。同一个数据目录只有一个引擎在同步、发信、发事件，因此不需要跨进程的
任务领取、租约或变更通知。

代价是 GUI 运行时 CLI 不能打开同一个数据目录。以后让其他前端通过 IPC 连接正在运行的引擎，
而不是各自打开数据库。

### 运行时桥接

GPUI 有自己的 executor，不是 tokio；IMAP / SMTP 栈却依赖 tokio reactor。`MailClient` 持有
（GUI：`MailClient::builder(cfg).open_async()` 自建）或借用（CLI：`.runtime(Handle::current())`）
一个 tokio runtime，每个公开方法把工作 `spawn` / `spawn_blocking` 到该 runtime 上再 await
`JoinHandle`，返回的 future 因此可以被任意 executor poll（`catmail-core/src/runtime.rs`）。

- `Runtime` 与 `Engine` 分放在两个 `Arc` 里，后台任务只持有 `Engine`，不会在 runtime 自己的
  线程上 drop 最后一个 runtime 引用。
- 自建 runtime drop 时 `shutdown_background()`，不等待任务：状态都已落 SQLite。借用的 runtime 不会
  被关掉，所以只持 `Weak<Engine>` 的后台任务（发信、GC）还 select 引擎的关闭信号（`Engine::closing`，
  随引擎 drop），客户端一没就结束，不等下一次定时器；每账户的后台同步靠账户的取消信号，同样随引擎 drop。
- `open` 在调用线程上加锁、迁移数据库、恢复中断的发送；有 UI 线程的前端用 `open_async`，把这些放到
  runtime 的 blocking pool 上，返回的 future 同样可以被任意 executor poll。
- 后台任务 panic 转成 `Error::Task`，GUI 进程不跟着崩。

### 错误

`catmail_core::Error` 是前端看到的唯一错误类型。用户能处理的失败有自己的变体（`AuthFailed`、
`Unsupported`、`AlreadyExists`、`OutOfSync`、各种 `*NotFound` 等），从下层错误里提升上来；其余的
store / IMAP / SMTP 错误包在不透明的 `StoreError` / `ImapError` / `SmtpError` 里，下层的错误枚举
不属于公开 API。

- 前端按 `Error::kind()`（`ErrorKind`：`Network`、`Auth`、`Refused`、`OutOfSync`、`NotFound`、
  `Invalid`、`Conflict`、`Internal`）决定图标和动作，不去解析 `Display` 文本。`is_transient()`：
  什么都不改、稍后重试可能成功（网络、SMTP 4xx）；`is_refusal()`：命令失败但连接仍可用，同一连接上
  其余的工作照常继续（引擎内部"记日志、跳过这个文件夹"的判断）。
- 后台工作的错误以 `ReportedError { kind, message, transient }` 交给前端：`ServerProblem.error`
  （账户状态）、`FolderSyncReport.error` 和 `ReplayReport.error`。登录被拒绝就是 `kind == Auth`，
  没有单独的布尔位；`transient` 是 `is_transient()` 的结果，因为单看种类分不出 SMTP 的 421 和 554。
- `ErrorKind` 故意不加 `#[non_exhaustive]`：以后加种类时，前端的 `match` 会编译失败，提醒每处都处理。

### 规则在引擎，措辞在前端

前端要预先知道引擎会怎么做（按钮是否可用、要不要先确认），这些判断由 types / core 给出，前端不自己
推导，否则引擎的规则一变，前端就静默偏离（以后的 IPC 前端还会再抄一遍）：

- 特殊文件夹：`special_folder`（某角色用哪个文件夹：可选中、id 最小）、`DeleteOutcome`（移到 Trash /
  已在任一 Trash 中永久删除 / 没有 Trash）、`MovePlan`（移动、归档改变位置还是标签），都作用于账户的文件夹列表。store 的
  `by_role`、`delete_messages`、`archive_messages` 调用的是同样这几个函数，规则只写一处。
- outbox：`OutboxStatus::went_out / needs_user / can_retry / can_mark_sent / can_cancel /
  retry_may_duplicate`，store 的状态转换也用它们判断。
- `FolderRole::label / counts_unread`，以及 `catmail_core::display`（大小、日期、"姓名否则邮箱"、
  地址列表、"(no subject)"）。

措辞、图标、布局留在各前端。

### 事务与事件

数据库访问只经过 `Store::read` / `Store::write`：闭包里的各个 repository 共用同一个事务；
`write` 是唯一写连接上的 `IMMEDIATE` 事务，闭包返回 `Ok` 才提交；`read` 在只读连接池上读最后一次
提交的快照，不等进行中的写（见[存储](architecture/storage.md#sqlite)）。必须原子的东西（本地改动与它的
`pending_ops` 行、同步来的邮件与文件夹游标）放在同一个闭包里。

事件（`tokio::sync::broadcast<Event>`）的规则：

1. **先提交，再发事件**，绝不在 `write` 闭包内发，否则订阅者可能读到尚未提交的状态。
2. 由执行变更的后台任务发出，调用方中途 drop future 也不会丢事件。
3. 只携带 ID（"某账户的某文件夹变了"），前端收到后重新查询；`RecvError::Lagged` 时整体刷新，
   不会丢状态。

### 一行是服务器上的一封邮件，UID 绑定 UIDVALIDITY

`messages` 的一行是服务器上的一封邮件，与服务器的语义一致：flags、keywords、MODSEQ 都是这一行
自己的状态。普通 IMAP 账户上它就是一个（文件夹, UID）对，同一封信复制到 Inbox 和 Archive 是两封
邮件、两行，一封已读、一封未读都可以；不按 Message-ID 合并（它不可靠），相同的原文由内容寻址的
BlobStore 自然只存一份。

Gmail 的文件夹是标签，同一封邮件出现在几个邮箱里。决定改为按 `X-GM-MSGID` 一封一行：它有一个位置
（"所有邮件"、已删除或垃圾邮件里的 UID），其余邮箱是它的标签。普通账户是没有标签的特例，两者是同一套
模型，见[位置与标签](architecture/labels.md)。

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

引擎为每个账户保留一份 `AccountState`：同步锁（同步、操作回放与后台同步的每一步串行；完整同步按文件夹拿锁，等着的同步在文件夹之间插进来）、发送锁
（同一条 outbox 不会被同时处理）、按需下载用的小连接池、唤醒后台同步的信号、"已删除"标记，以及
最近几次连接服务器的结果（`MailClient::account_status`：同步、发信各自是否连续失败、是否登录被拒绝，
变化时发 `AccountStatusChanged`）。这份状态只由对账户的实际工作创建：前端查询一个不存在的 id
（`account_status`）不会留下条目，没有后台同步时也不记录草稿的编辑时间。连接和登录失败属于账户，不算在某封信或某次操作头上。删除账户时先置标记、清空连接池，进行中
的同步和下载立即返回 `Error::Cancelled`，不用等网络超时，然后才删除数据；这份状态作为墓碑
保留（账户 id 不复用），之后对它的任何操作立即取消。

开启后台同步（GUI、`catmail watch`）时，每个账户还有一个后台任务和它自己的一条连接：定时同步全部
文件夹，其间在 INBOX 上 IDLE（服务器不支持时轮询），本地改动入队后立即回放（见
[同步 / 后台同步](architecture/sync.md#后台同步与-idle)）。它与手动操作共用上面这些锁，而不是做成独占
全部连接、经 mpsc 接收命令的 actor：门面的方法已经按锁串行，按需下载有连接池，都经一个 actor 转发
不增加任何保证，反而让打开邮件排在同步后面。

### 安全边界

- **凭据**：密码只在系统密钥环里，不进 SQLite（账户的和各发件身份的）。添加先写密钥再插入行；删除先删行
  （`RETURNING secret_key`）再删密钥，并发删除不会留下孤立凭据。
- **TLS**：rustls + 操作系统的信任库；配置了 STARTTLS 就必须成功，绝不降级为明文。
- **邮件内容不可信**：HTML 入库前经 ammonia 清洗，样式表由 cssparser 逐条重写（不能取资源、
  不能结束 `<style>`）；远程图片默认不加载，GUI 读信不发出任何网络
  请求，除非用户对这封信点了"Show remote images"，下载也由引擎做（`MailClient::fetch_remote_images`：
  不带 cookie 和 referrer，只 GET）；`catmail-render` 的引擎没有网络栈，一切资源只经读信区给的函数；服务器给的值（BODYSTRUCTURE、msg-id）写进邮件头之前都要校验。
- **本地数据**：数据目录只有本用户可访问，交给其他程序的文件也放在里面。

## 子系统

| 文档 | 内容 |
|---|---|
| [同步](architecture/sync.md) | 文件夹对账、UIDVALIDITY、增量拉取、游标与断点续传、容错、邮件头 |
| [操作队列](architecture/op-queue.md) | 标记 / 移动 / 删除的本地效果、回放顺序、拒绝时的撤销、无 MOVE 服务器上的 COPY |
| [正文与附件](architecture/bodies.md) | 按需下载的两条路径、正文提取、HTML 清洗、内嵌图片 |
| [草稿与发信](architecture/sending.md) | 草稿与回复、草稿同步到服务器、outbox 状态机、SMTP 客户端、重试、保存到 Sent |
| [位置与标签](architecture/labels.md) | Gmail 按标签建模：一封邮件一行、位置与标签、同步、操作与界面 |
| [IMAP / SMTP 连接](architecture/protocol.md) | IMAP 库（改用 imap-proto 的决定）、连接层的严格解析、连接池、认证、TLS |
| [存储](architecture/storage.md) | SQLite 约定、计数、跨账户约束、全文搜索、BlobStore 与 GC、凭据 |
| [CLI](architecture/cli.md) | 命令的代码结构、`--json` 的结构与版本、退出码、添加账户前的登录检查、不覆盖文件 |
| [GUI](architecture/gui.md) | 视图结构、事件刷新、读信与写信窗口、发件箱、主题与设置、HTML 渲染与图片策略、已知问题 |
| [后台运行](architecture/background.md) | 关窗后常驻、单实例与唤起、睡眠唤醒、新邮件事件；通知、托盘、开机启动（设计中） |
| [日志](architecture/logging.md) | stderr 与数据目录里的日志文件、谁能写、记什么、轮转 |
| [测试](architecture/testing.md) | 假 IMAP / SMTP 服务器、真实 Dovecot 与 mailpit、GUI 测试 |

## 数据目录

`Config::platform_default()`（`directories`）：Linux `~/.local/share/catmail`，macOS
`~/Library/Application Support/net.yangkx.catmail`，Windows `%APPDATA%\yangkx\catmail\data`。
CLI 和 GUI 都可用 `--data-dir` 或 `CATMAIL_DATA_DIR` 覆盖。

| 路径 | 内容 |
|---|---|
| `catmail.db` | SQLite：账户、文件夹、邮件头与正文、草稿、队列 |
| `blobs/` | 原文、附件等内容寻址文件 |
| `lock` | 引擎的独占锁 |
| `gui.sock` | 之后的 GUI 启动唤起正在运行的那个所用的 socket（路径太长时在运行时目录，见[后台运行](architecture/background.md#单实例与唤起)） |
| `handoff/` | 前端交给其他程序的文件（如给浏览器的邮件页面） |
| `remote-images/` | 用户要求显示过的远程图片的缓存（见[正文与附件](architecture/bodies.md)） |
| `logs/` | 前端的日志文件和它的锁（见[日志](architecture/logging.md)） |

目录新建时只有本用户可访问（Unix 上 0700）。已存在的目录不改权限：它可能是用户指定的位置（比如
家目录），宽于 0700 时只打一条 warn；但必须属于当前用户，否则拒绝打开（`Error::DataDirNotOwned`），
这也挡住了别人在共享位置预先放置的、指向他自己目录的符号链接（指向自己目录的链接照常可用）。里面的
东西不依赖外层目录：`catmail.db` 及其 WAL 文件是 0600，`blobs/`、`blobs/tmp`、`handoff/`、`remote-images/` 是 0700，
已存在的在打开时收紧，blob 和交接文件本身是 0600。Windows 上一律继承父目录的 ACL。`MailClient::open`
拿到锁之后清空并重建 `handoff/`：锁保证没有前端还在往里写，各前端不必各自清理。在打开时而不是关闭时
清，是因为关闭时别的程序可能还在读，macOS 上应用退出也不执行析构。前端不自己写这个目录，而是调
`MailClient::hand_off`：它建 0600 的文件，并顺手删掉超过 10 分钟的旧文件，免得 GUI 开几天就留下几天的邮件。
