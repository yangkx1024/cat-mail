# 草稿与发信

[← 架构](../architecture.md)

## 草稿

`drafts` 一行是正在写的一封信（收件人、抄送、密送、主题、纯文本正文）。草稿目前只在本地；同步到
服务器的 Drafts 文件夹（`remote_*` 列）每次保存都要删除上一份副本，等操作队列支持后再做。

- **附件**在添加时复制进 BlobStore（`draft_attachments`），原文件之后改动或删除都不影响。
- **地址**在保存时检查（`parse_addresses`）：逗号或分号分隔，每一项只能是裸地址或
  "显示名 <地址>"，严格解析，任何一项不合规就报错，不会悄悄少一个收件人；地址必须能原样放进
  `MAIL FROM:<…>` / `RCPT TO:<…>`。非 ASCII 地址需要 SMTPUTF8，暂不支持。
- **回复**（`MailClient::reply`）：发给原信的 Reply-To，没有时发给发件人；回复自己发出的信则发给
  原收件人。回复全部再加上原信的收件人和抄送，去掉自己、去重（地址不分大小写）。主题加 `Re: `
  （已以 `Re:` / `回复：` / `答复：` 开头的不加），正文引用原文（`> `），正文没下载时先下载。原信
  的 Message-ID 和 References 复制进草稿（`in_reply_to` / `references_hdr`），原信之后被删也不会
  断开会话；发出后经操作队列给原信加 `\Answered`。

## Outbox 快照

`send_draft` 把草稿冻结成 outbox 的一行：

- 完整的 MIME 原文此时构建（mail-builder：7-bit、CRLF、RFC 2047 头、RFC 2231 文件名；
  Message-ID 为 `<uuid@发件人域名>`，不泄露主机名；msg-id 再按收信时的规则检查一遍）并存成 blob，
  之后每次尝试都发这同一份字节，重试不再读草稿。
- SMTP 信封 `envelope_from` 和 `recipients_json`（所有 RCPT TO，含不在 MIME 头里的密送）单独存。
- 构建在事务外进行，入队时检查草稿的 `revision`（每次修改、增删附件都加一）没变，期间被改过就报
  `Conflict`，不会把旧内容发出去。
- 有 outbox 行的草稿被锁定：不能编辑、删除，不在草稿列表里。发完后随 outbox 行一起删除；取消没发
  出去的发送只删 outbox 行，草稿回到可编辑状态。

## 状态机

`crates/catmail-core/src/send.rs`，转换都是 `Outbox` repository 里的条件 UPDATE。

| 状态 | 含义 | 之后 |
|---|---|---|
| `queued` | 等待发送（`next_attempt_at`） | → `sending`（算一次尝试） |
| `sending` | SMTP 事务进行中，结束标记还没发 | 可重试 → `queued`，永久 → `failed`；发结束标记**之前**先提交 → `unknown` |
| `unknown` | 结束标记已发（或正要发），还没收到答复 | 2xx → `accepted`；4xx / 5xx → `queued` / `failed`；其他答复、断线、超时、进程崩溃都**停在这里** |
| `accepted` | 服务器已接受，待保存到 Sent | 只重试 APPEND，**绝不重发 SMTP** → done |
| `failed` | 被永久拒绝，原因在 `last_error` | 由用户处理 |
| done | 已发送，需要时已保存到 Sent | 删除这一行和草稿 |

服务器只有收到结束标记 `.` 才可能接受邮件，所以 `unknown` **先写**：那之前出的错都能安全重试；
从写下 `unknown` 起不会自动重发。用户可以重试（可能收到两封）、确认已送达（`mark_outbox_sent`，
之后只保存到 Sent）或取消。

- 用户的这些决定先拿该账户的发送锁：发送中的条目在等答复时就是 `unknown`，此时改动它，随后到来的
  250 就记不下来了。所以等这次尝试结束，决定作用于它留下的状态。
- 残留的 `sending` 说明尝试死在结束标记之前，退回 `queued`：打开引擎时（拿到数据目录锁后）全部
  处理一次；拿到某账户的发送锁时也处理该账户的（写库失败、flush 被取消等），不必等到重启。
- **落盘。** SQLite 平时是 WAL + `synchronous=NORMAL`：进程崩溃不丢提交，断电却可能丢掉最近几次，
  `unknown` 会退回 `sending`、重启后被重发。所以 `unknown` 和保存到 Sent 的尝试计数用
  `Store::write_durable` 提交（只对这次提交 `FULL`，之前的提交一并落盘）。不全局开 `FULL`：同步时
  每批邮件都要提交，单连接上的读会排在 fsync 后面。

## SMTP 客户端

`catmail-proto/src/smtp/`，自己实现而不用 lettre：状态机要区分"结束标记发出之前 / 之后"的失败，
lettre 的 `send` 不暴露这一点；自己实现还能共用 `TlsConfig`、超时和 STARTTLS 的处理。一次事务拆成
`begin`（MAIL FROM、逐个 RCPT TO、DATA）、`write_message`（点填充后的正文，不含结束标记）、
`end_message`（结束标记并等答复，RFC 5321 允许服务器想 10 分钟），调用方在后两步之间提交 `unknown`。

- 任何一个收件人被拒绝就 RSET 整个事务，错误里带上那个地址：不会一部分人收到、一部分人没收到。
- 正文里孤立的 CR 或 LF 一律改成 CRLF 再做点填充，没有服务器能在别处看到结束标记（SMTP smuggling）。
- EHLO 用 `[127.0.0.1]`：地址字面量合法，又不暴露主机名和内网地址。
- 服务器声明 SIZE 时，超出的邮件直接失败；有 SIZE 时在 MAIL FROM 带上大小。
- 服务器不提供 AUTH 时不登录（本地中继）；STARTTLS 配置了就必须成功。
- EHLO 回 4xx 是暂时失败；5xx 才算不支持 ESMTP。
- 结束标记之后：2xx 都算接受；4xx / 5xx 是拒绝，服务器已丢弃邮件；其他答复按结果未知处理。

## 重试

- 4xx、断线、超时、读不到密码（密钥环被锁或没启动）是暂时失败，按 30 秒起、每次翻倍、最多 30 分钟
  的间隔一直重试，`last_error` 随时可见。5xx、认证失败、服务器缺少所需能力、邮件过大是永久失败。
- 一轮里连接服务器失败时，剩下排队的不再逐个尝试，推迟到和失败的那封同一时刻（不计尝试次数）：
  推迟得更早的话，下一轮由它们轮流去连，退避就失效了。同一轮的多封邮件复用一条 SMTP 连接。
- 已知的权宜之计：连接时的失败（连不上、登录、密码、TLS）属于账户而不是某一封信，现在却记在队首
  那封上，密码错误时每轮让一封变成 `failed`。开启后台发送前改成整轮推迟、在账户上报告。

## 保存到 Sent

被接受后在同一轮里立即 APPEND（带 `\Seen`），失败的留到之后几轮重试。目标优先用本地已知的 Sent
角色文件夹；账户还没同步过时用 LIST 按同步相同的角色规则现找；没有 Sent 就跳过。APPEND 走按需
下载的连接池。

APPEND 的答复也可能丢失，所以尝试次数在每次尝试**之前**加一：第一次直接 APPEND，第二次起（包括同一
次尝试里换新连接重来）先 EXAMINE Sent、按 Message-ID `UID SEARCH HEADER`，已经在了就不再 APPEND。

已知问题：Gmail、Outlook 等会自动把发出的邮件存进已发送，再 APPEND 会重复；它们需要 OAuth2，到时
按服务器关掉这一步。

## 谁来发

`MailClientBuilder::send_in_background(true)` 时引擎起一个后台任务，按 `next_attempt_at` 和入队、
重试的通知处理到期的条目（GUI 用）；它只持有 `Weak<Engine>`，不会让数据目录锁一直被占着。CLI 不开
后台发送，免得一个无关的命令在背后发信：`catmail send` 入队后立即 `flush_outbox`，失败的留给
`catmail outbox flush`。每账户一把发送锁（`Engine::send_permit`），后台发送和手动 flush 不会同时
处理同一条。

发送相关的事件：`DraftsChanged`、`OutboxChanged`；服务器接受一封邮件时另有 `MessageSent`。
