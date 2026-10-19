# M3：草稿、SMTP 发信与 outbox 状态机、保存到 Sent

- 日期：2026-10-19
- 类型：功能

## 背景

M2 之后客户端只能读信。M3 要能写草稿、回复、经 SMTP 发出并保存到服务器的"已发送"，
发送过程要经得起断网、服务器拒绝、确认丢失和进程中途退出，且绝不在用户不知情时重复投递。
CLI 提供 `catmail compose` / `reply` / `draft` / `send` / `outbox`；图形界面的写信留到 M5。

## 决策

- **自己实现 SMTP 客户端，不用 lettre。** outbox 状态机必须区分"结束标记 `.` 发出之前的
  失败"（服务器会丢弃，可以安全重发）和"之后的失败"（结果未知）。lettre 的 `send` 只给一个
  错误，不暴露这一点；自己实现还能共用 `TlsConfig`、超时和 STARTTLS 的处理（与 IMAP 一样
  升级后丢弃明文缓冲）。提交协议本身很小（EHLO、STARTTLS、AUTH、MAIL/RCPT/DATA）。
- **MIME 用 mail-builder 构建**（与 mail-parser 同一家），关掉默认的 `gethostname`：
  Message-ID 由引擎用 `uuid@发件人域名` 生成，不泄露主机名。
- **`unknown` 先写**：发结束标记之前先提交 `unknown`，于是断线、超时、进程被杀都停在
  `unknown`，只有用户能让它重发；启动时残留的 `sending` 说明死在结束标记之前，退回队列。
  比"发完再记录"少一个会导致重复投递的窗口。
- **草稿只在本地。** 同步到服务器的 Drafts 文件夹需要每次保存都删除旧副本，属于 M4 的操作
  队列；`drafts.remote_*` 列保留。回复时把 Message-ID / References 复制进草稿，原信被删也
  不影响会话。
- **草稿入队后锁定**（有 outbox 行即锁定，不另加状态列）：发完连同 outbox 行删除；取消没
  发出去的发送只删 outbox 行，草稿回到可编辑。MIME 在事务外构建，入队时比对草稿的
  `revision`，期间被改过就报 `Conflict`。
- **重试**：暂时失败按 30 秒起翻倍、最多 30 分钟一直重试（离线优先，不因离线太久就判失败），
  `last_error` 随时可见；5xx、认证失败、缺少所需能力、超出 SIZE 为永久失败。连不上服务器时
  本轮剩下的邮件只推迟、不计次数。任何一个收件人被拒就取消整封（RSET），不出现部分投递。
- **保存到 Sent 的去重**：APPEND 的答复也会丢，所以尝试次数在每次尝试之前加一，第二次起先按
  Message-ID `UID SEARCH HEADER`，已存在就不再 APPEND。没同步过的账户用 LIST 现找 Sent；
  没有 Sent 就跳过。Gmail / Outlook 会自动存已发送，届时（M6，OAuth2）按服务器关掉这步。
- **后台发送是可选的**：`send_in_background(true)` 给 GUI 用；CLI 不开，免得 `catmail ls`
  之类的命令在背后发信，`catmail send` 当场 flush。后台任务只持有 `Weak<Engine>`。
- **非 ASCII 地址暂不支持**（需要 SMTPUTF8），保存草稿时就拒绝。
- **地址解析放在 core**（`parse_addresses`，基于 mail-parser），CLI 和以后的 GUI 写信框共用。
- **messages 表加 `reply_to_json`**，同步时多取 REPLY-TO 头，回复邮件列表时用。
- **实测发现的问题**：Dovecot 无法写入时对 APPEND 不回 `+` 而直接回 tagged NO；imap-next
  只在 BAD 时放弃带 literal 的命令，收到 NO 后这条连接上之后的命令全部卡住，放回连接池后
  下一次使用要等 60 秒超时。现在命令完整发出前收到 NO/BAD 返回致命的 `Error::Aborted`，
  连接不再复用；假服务器加了同样的故障，有不修就超时的测试。

- **/simplify 清理**（四个方向的审查）：IMAP / SMTP 共用 `net.rs` 里的建连、TLS 和超时；
  信封地址规则只在 proto 一处（`smtp::is_envelope_address`），core 只额外排除头里会拆开
  地址的字符；SMTP 正文分 64 KiB 块做点填充，不再复制整封邮件；被接受后立即保存到 Sent，
  不再读第二遍；先连接再读邮件；每次状态转换后从数据库读回条目，不再手抄状态；多个账户
  的后台发送并发进行；连接池只在连接断开时重试（被拒绝的 APPEND 不再发两次），断步的 IMAP
  连接自己拒绝后续命令；`Address` 实现 `Display`，CLI / GUI / 引用头共用。放弃了：让
  mail-builder 直接流式写入 blob、Sent 文件夹每轮只解析一次、草稿列表的 N+1 查询、把状态机
  的每条边都统一成一个 `transition`、把不入库的 `Done` 从 `OutboxStatus` 拆出去（改动面
  大，收益小）。

- **codex review 修复**（4 条，均已核实；前 3 条先写出不修就失败的测试）：
  - 用户的重试 / 确认送达 / 取消不拿发送锁：在等服务器对结束标记的答复时（已是 `unknown`）
    重试会把它改回 `queued`，随后的 250 记不下来，保存路径又把它改成 `sending`，卡到重启后
    被重新发送。现在这些决定先拿账户的发送锁，保存也只对确实 `accepted` 的条目进行。假 SMTP
    加了暂扣结束标记答复的闸门。之后把 `begin_attempt` 拆成 `begin_send` / `begin_save`，
    各自只接受对应的原状态，保存路径不再需要调用方判断状态。
  - 回复时复制的 Message-ID 若含裸 CR（mail-parser 会保留），发出时被规范化成 CRLF，可注入
    邮件头。解析邮件头时就丢掉不合法的 msg-id（入库的都是干净的），构建邮件时再按同一规则
    检查一遍；SMTP 信封地址也用这条规则。
  - `bob@example.com, Carol` 会被宽松的头解析器丢掉 Carol 而不报错。第一次只改成按逗号
    分段、每段仍交给头解析器，`bob@example.com; Carol` 照样丢人（复核时发现）。现在不再用
    头解析器：逗号或分号分隔，每项严格按"裸地址 / 显示名 <地址>"解析，其余一律报错。
  - Windows 上 `File::open` 打开目录直接报权限错误，附件"不是文件"的检查走不到（Windows CI
    的测试会失败）。现在先查元数据再打开。

## 方案

- `catmail-types`：`Draft`、`DraftContent`、`DraftAttachment`、`OutboxEntry`、`OutboxStatus`
  及 ID；事件 `DraftsChanged`、`OutboxChanged`、`MessageSent`；`Message` 加 `reply_to`、
  `references`。
- `catmail-store`：`0001_init.sql` 改 drafts（`revision`、`in_reply_to`、`references_hdr`）、
  draft_attachments（`size`）、outbox（`message_id_hdr`、`subject`，`draft_id` 唯一索引）、
  messages（`reply_to_json`）；新 repository `Drafts`、`Outbox`（状态转换都是带原状态条件的
  UPDATE），`Folders::remote_name_by_role`。
- `catmail-proto`：`smtp/`（`Session::connect` / `login` / `begin` / `write_message` /
  `end_message`）；`mime/build.rs`（`build_message`）；IMAP `append`、
  `uid_search_message_id`、`Error::Aborted`；`Credentials` 移到 crate 根，IMAP / SMTP 共用。
- `catmail-core`：`drafts.rs`（草稿、附件、回复）、`send.rs`（入队、flush、后台任务、
  用户操作）、每账户发送锁、打开时恢复 `sending`；`MailClient` 新增草稿 / 发信 / outbox 方法。
- `catmail-testkit`：假 SMTP 服务器（AUTH、SIZE、STARTTLS，故障：421、拒绝登录、拒收件人、
  451、MAIL 时断线、结束标记回错误码、收下后不答复）；假 IMAP 加 APPEND、`SEARCH HEADER`、
  UIDPLUS 及 APPEND 故障。
- `catmail-cli`：`compose.rs`；错误输出保留换行（用于提示下一步命令）。
- 验证：store 的状态机单测；proto 的 SMTP / APPEND 测试；core `tests/send.rs` 16 个场景
  （正常发送、确认丢失不重发、未知结果的重发与取消、退避重试、连不上时推迟、连接中断后
  重试、收件人被拒后修改重发、密码错误、保存失败只重试保存、APPEND 答复丢失不重复保存、
  APPEND 被提前拒绝不卡连接、无 Sent / 未同步、回复的线程头、附件、草稿检查、后台发送）；
  重启恢复的单测；Dovecot 测试里经假 SMTP 发信并保存到真实的 Sent（含一次失败后的去重
  搜索）。手动用 CLI 对 Dovecot + 本地 SMTP 接收端走了一遍写信、回复、断网发送和恢复。
