# 草稿与发信

[← 架构](../architecture.md)

## 草稿

`drafts` 一行是正在写的一封信（收件人、抄送、密送、主题、纯文本正文）。它同时在服务器的 Drafts
文件夹里有一份副本，见[同步到服务器](#同步到服务器)。

- **附件**在添加时复制进 BlobStore（`draft_attachments`），原文件之后改动或删除都不影响。
- **地址**在保存时检查（`parse_addresses`）：逗号或分号分隔，每一项只能是裸地址或
  "显示名 <地址>"，严格解析，任何一项不合规就报错，不会悄悄少一个收件人；地址必须能原样放进
  `MAIL FROM:<…>` / `RCPT TO:<…>`。非 ASCII 地址需要 SMTPUTF8，暂不支持。
- **NUL** 在保存时从主题、正文和显示名里去掉：它在文本里没有意义，而 IMAP literal 不能带，存进草稿
  会让每次上传失败（以前还会让整个账户的同步失败，因为上传在同步之前）。引用一封带 `=00` 的来信
  就会遇到。
- **回复**（`MailClient::reply`）：发给原信的 Reply-To，没有时发给发件人；回复自己发出的信则发给
  原收件人。回复全部再加上原信的收件人和抄送，去掉自己、去重（地址不分大小写）。主题加 `Re: `
  （已以 `Re:` / `回复：` / `答复：` 开头的不加），正文引用原文（`> `），正文没下载时先下载。原信
  的 Message-ID 和 References 复制进草稿（`in_reply_to` / `references_hdr`），原信之后被删也不会
  断开会话；发出后经操作队列给原信加 `\Answered`。
- **转发**（`MailClient::forward`）：新草稿，没有收件人；主题加 `Fwd: `（已以 `Fwd:` / `Fw:` /
  `转发：` 开头的不加），正文是空的两行、`---------- Forwarded message ---------` 和原信的 From /
  Date / Subject / To / Cc，再接原文（不加 `> `）。原信的附件（不含 HTML 里显示的内嵌图片）复制进草稿；
  正文和附件没下载的先下载。全部下载完、内容放进 BlobStore 之后，草稿和它的附件在**同一个事务**里写入，
  任何一个附件取不到就什么也不留；`put` 紧挨着这个事务，GC 的宽限期覆盖其间。不设 In-Reply-To /
  References：转发开始的是另一段对话。

## 同步到服务器

`catmail-core/src/draft_sync.rs`。草稿的 `revision` 与副本的版本（`remote_revision`）不同、又不在
outbox 里时，上传一份新的，再删除旧的：

- **时机**：与操作队列的回放相同（`ops::replay_queued`，持有同步锁）：同步对账完文件夹之后、回放之前；
  `replay_pending`；开启后台同步时，草稿开始被修改时唤醒后台任务（让它算出何时上传），接着的修改只把
  时间往后推，不再每次唤醒。上传在回放之前，被替换的旧副本
  在同一次回放里删除。
- **后台上传等草稿停下来**：GUI 边写边自动保存，每次保存都换一份副本没有意义。后台同步只上传
  `draft_upload_delay`（默认 15 秒，`MailClientBuilder::draft_upload_delay`）内没改过的草稿，被留下的
  草稿到期时后台任务自己醒来（IDLE 的时限取它和下一次完整同步中较早的一个）。最后修改时间只记在内存里
  （`Engine::draft_edited`，按草稿），重启后没有记录的草稿直接上传。用户要求的上传
  （`replay_pending`、`sync_account`）不等，CLI 的行为不变。
- **上传**：完整的 MIME（与发信同一个构建函数，但**保留 Bcc 头**，否则重新打开草稿会丢掉密送），
  flags 为 `\Draft \Seen`，每次一个新的 Message-ID。先 APPEND 新的、再删旧的，服务器上始终有一份。
  旧副本与新副本在同一文件夹、同一代际（APPENDUID 刚报告的 UIDVALIDITY）时，按 UID 经操作队列删除
  （见[操作队列](op-queue.md#草稿的远端副本)）。
- **答复丢失**：APPEND 之前用 `write_durable` 记下这次上传（`draft_uploads`：草稿、文件夹、Message-ID、
  版本）。有 APPENDUID 就直接结算；没有答复（断线）的留到下一轮，**先于任何新上传**在文件夹里按
  Message-ID 搜索：找到了就是这次上传的副本，没找到说明没做成。没有 UIDPLUS 的服务器也靠这个搜索拿到
  UID。重试因此不会多出一份。
- **按 Message-ID 删除**：其余要删的副本都转成一条没有草稿的上传（`draft_id` 为空），下一轮按
  Message-ID 找到后删除：删除草稿、发出后（outbox 完成）、被替换的旧副本属于另一代际或另一文件夹时。
  本地记下的 UIDVALIDITY 可能已经过时（文件夹被重建、还没有同步发现），按它删除会变成冲突、副本留下。
  上传期间草稿被删除或发出也一样：`draft_uploads.draft_id` 置空，结算时找到的副本直接删除。
- **结算顺序无关**：比草稿现有副本旧的上传只删除它找到的东西，不会顶替较新的副本。
- **UIDVALIDITY 改变**：副本的 UID 作废，但服务器多半只是重新编号，副本还在。这时副本转成一条待结算的
  上传，按它的 Message-ID 找回并记下新 UID，不重新上传、也不留下重复的一份；找不到再重新上传。
- **被拒绝**（如超出配额）：APPEND 被拒绝时什么也没存，原因记在 `drafts.upload_error`，下一轮再试，
  成功后清除。在发出邮件之前就被拒绝（Dovecot 无法写入时不回 `+`，见 [IMAP](protocol.md)）同样是
  拒绝，但那条连接已经不能用：记下原因后换一条新连接继续，否则上传在同步之前，这一封草稿会让账户的
  每次同步都失败。答复是 OK 但有一行解析不了（`Incomplete`）不算拒绝：邮件已经存下，按 Message-ID
  结算。构建失败（附件的 blob 不见了）同样记录，不挡住其他草稿和操作的回放。
- 账户没有 Drafts 文件夹（或还没同步过、不知道有）时草稿只在本地，有了以后由同步上传。
- 只单向上传：Drafts 里其他客户端写的草稿是普通邮件，不变成本地草稿；我们的副本在本地也作为 Drafts
  文件夹里的邮件出现（同步拉下来）。

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
  250 就记不下来了。所以等这次尝试结束，决定作用于它留下的状态。例外：取消一封还在排队（`queued`）
  或已失败（`failed`）的不等锁，立即删除（`Outbox::withdraw`，带状态条件的 DELETE）。一轮发送从头到尾
  持有发送锁，等它结束时这一轮早把排在后面的那封发出去了；而尝试只从 `queued` 开始、且开始本身是
  带条件的 UPDATE（`begin_send`），两者不会交错。
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

- 一轮先连上并登录 SMTP 服务器，再开始第一封（`begin_send`），同一轮的多封邮件复用这条连接。连接时
  的失败（连不上、TLS、登录被拒绝、读不到密码、服务器缺少所需能力）属于账户而不是某一封信：这一轮
  所有排队的都推迟到同一时刻，不计尝试次数，`last_error` 写上原因；间隔按**账户**连续失败的次数从
  30 秒起翻倍、最多 30 分钟（失败次数记在账户状态里，见下）。否则密码错误时每轮会让一封变成 `failed`，
  打开后台发送后整个发件箱就在无人值守时逐封失败了。
- 开始之后的 4xx、断线、超时是这封信的暂时失败，按它自己的尝试次数同样退避；5xx、服务器缺少所需能力
  （如 SIZE 超限）、邮件过大是永久失败。
- **账户状态。** 连接成功或失败都记进引擎的每账户状态（`MailClient::account_status` 的 `send`），变化
  时发 `AccountStatusChanged`，前端据此提示"密码被拒绝"等，不必从某一封信的 `last_error` 去猜。
- 一轮中途出错（例如写库失败）时，已经做完的也照样发 `OutboxChanged`。

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
