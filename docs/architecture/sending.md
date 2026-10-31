# 草稿与发信

[← 架构](../architecture.md)

## 草稿

`drafts` 一行是正在写的一封信（收件人、抄送、密送、主题、纯文本正文）。它同时在服务器的 Drafts
文件夹里有一份副本，见[同步到服务器](#同步到服务器)。

- **附件**在添加时复制进 BlobStore（`draft_attachments`），原文件之后改动或删除都不影响。
  一封草稿的附件合计有上限（见下），超出报 `InvalidInput`：复制前按文件大小先拒一次，复制最多读到
  剩余额度多一个字节，超出额度就拒绝（文件读的时候变大也一样），不存下截断的内容：期间草稿腾出了空间
  也不能让一部分冒充整个文件。被拒的内容留给 GC。转发带来的原信附件不受这个限制，但计入之后添加的附件。
  - 已有的草稿经 `attach_file` 添加（GUI、`catmail draft attach`）：写入草稿的事务里再按存下的大小
    查一次合计（期间可能有别的附件加进来）。
  - `create_draft` / `reply` / `forward` 也接受要附上的文件（CLI 的 `--attach`）：按新草稿将用的身份
    算上限（`Drafts::new_size_limits`），草稿就用这个身份建（没指定时先定下当时的默认身份：复制期间
    默认身份可能被改，再解析一次会把附件放到没检查过的身份上），复制完后和草稿在同一个事务里写入，所以草稿一出现就带着附件，
    不会每个附件多一个版本、多一次上传到 Drafts；哪个文件不行就报 `Error::Attach`（带上路径），
    不建草稿。草稿还不存在，没有别人能往里加附件，不必再查一次。
- **附件上限**按服务器说的算（`attachments_limit`）：发这封草稿的 SMTP 服务器（草稿所选身份的）
  在 EHLO 里公布的 `SIZE`（RFC 1870），和账户 IMAP 服务器的 `APPENDLIMIT=<n>`（RFC 7889；Drafts 和
  Sent 的副本要 APPEND 进去），取小的，各自先减去 64 KiB 的头和正文余量、再按 base64（每 57 字节一行
  78 字符）折算。只是估算，正文很长时发送仍可能因 `SIZE` 失败，那一步照常检查。
  - 不知道（还没连过、服务器不公布、`SIZE 0`）时 SMTP 一侧用 25 MiB：Gmail、Outlook 和大多数服务器
    允许的附件大小，编码后约 35.87 MB，正好在 Gmail 的 `SIZE 35882577` 以内（按 Gmail 的值算出来的是
    24.96 MiB）。
  - 无论服务器收多大都不超过 50 MiB：整封信在内存里构建，每次修改后还要整封复制到 Drafts。
  - `APPENDLIMIT=0`（RFC 7889 §5：根本不收 APPEND）由 `Capabilities::append_limit` 照实返回 0，但
    记录时（`connection::announced_append_limit`）不当作大小：副本多小都存不进去，没有要放得下的东西，
    限制附件也帮不上，发信本身不受影响。
  - 值记在 `identities.smtp_max_size` 和 `accounts.imap_append_limit`，**每次连接**都与
    服务器现在说的比较，不同就更新（服务器会调整上限，也会不再公布）：发信连 SMTP、同步等连 IMAP 时。
    添加时登录检查（`Verify::Login`）拿到的值由 `add_account` / `add_identity` 在插入的同一事务里写进去，
    第一次发信之前就知道；`Verify::Skip` 添加的要等第一次连接。记录失败只写日志，
    不影响连接。几个身份共用一条 SMTP 登录时（见[重试](#重试)），连接时学到的 SIZE 记到每一个身份上。
- **地址**在保存时检查（`parse_addresses`）：逗号或分号分隔，每一项只能是裸地址或
  "显示名 <地址>"，严格解析，任何一项不合规就报错，不会悄悄少一个收件人；地址必须能原样放进
  `MAIL FROM:<…>` / `RCPT TO:<…>`。非 ASCII 地址需要 SMTPUTF8，暂不支持。
- **NUL** 在保存时从主题、正文和显示名里去掉：它在文本里没有意义，而 IMAP literal 不能带，存进草稿
  会让每次上传失败（以前还会让整个账户的同步失败，因为上传在同步之前）。引用一封带 `=00` 的来信
  就会遇到。
- **回复**（`MailClient::reply`）：发给原信的 Reply-To，没有时发给发件人；回复自己发出的信则发给
  原收件人。回复全部再加上原信的收件人和抄送，去掉自己、去重（地址不分大小写）。主题加 `Re: `
  （已以 `Re:` / `回复：` / `答复：` 开头的不加），正文引用原文（`> `），调用方给的文字写在引文上方，
正文没下载时先下载。原信
  的 Message-ID 和 References 复制进草稿（`in_reply_to` / `references_hdr`），原信之后被删也不会
  断开会话；发出后经操作队列给原信加 `\Answered`。`MailClient::can_reply_all` 用同一套收件人规则判断
  回复全部是否比回复多出收件人（抄送不为空），只读头部、不下载正文，界面据此决定是否显示 Reply all。
- **转发**（`MailClient::forward`）：新草稿，收件人和写在转发内容上方的文字由调用方给（`Forwarding`，
  默认都为空；地址在下载之前就检查）；主题加 `Fwd: `（已以 `Fwd:` / `Fw:` /
  `转发：` 开头的不加），正文是空的两行、`---------- Forwarded message ---------` 和原信的 From /
  Date / Subject / To / Cc，再接原文（不加 `> `）。原信的附件（不含 HTML 里显示的内嵌图片）复制进草稿；
  正文和附件没下载的先下载。全部下载完、内容放进 BlobStore 之后，草稿和它的附件在**同一个事务**里写入，
  任何一个附件取不到就什么也不留；`put` 紧挨着这个事务，GC 的宽限期覆盖其间。不设 In-Reply-To /
  References：转发开始的是另一段对话。

## 发件身份

`identities`：账户发信用的地址，各有自己的 SMTP 服务器和用户名。账户本身只是 IMAP 邮箱；添加账户时
用 `NewAccount::smtp` 建出它的第一个身份（账户的地址、名字和用户名，`secret_key` 为空：用账户的密码
登录）。之后可以再加，例如经 Resend 这类只提供 SMTP 的服务发信，或给账户的地址换一套 SMTP 登录；这些各有
一个 keyring 条目（见[存储 / 凭据](storage.md#凭据)）。收信、Drafts 和 Sent 仍是账户的 IMAP。

- **默认身份**：每个账户恰好一个（`is_default`，部分唯一索引保证），新草稿从它发；可以换
  （`set_default_identity`，在一个事务里先清掉旧的再设新的），不能删除。列出时默认的在最前，其余按添加
  先后。
- **草稿选身份**：`drafts.identity_id` 必填，读出的 `Draft::identity` 总是一个具体的身份。身份不在
  `DraftContent` 里：只有新建（`create_draft(account, from: Option<IdentityId>, ..)`）可以不指定，存成
  账户当时的默认身份；`update_draft(id, from: IdentityId, ..)` 必须给出，自己拼一个 `DraftContent` 的
  调用方不会无意中把发件人换回默认。`reply`、`forward` 也可以指定身份（`from: Option<IdentityId>`），
  和正文、转发的收件人一样在建草稿时就定下，不再事后 `update_draft`；不指定时回复按原信选，转发用默认身份。建草稿时由 repository
  检查身份属于这个账户（复合外键 `(identity_id, account_id)` 兜底）；改身份和改别的内容一样算一次修改。From 头、MAIL FROM 和
  Message-ID 的域名都来自所选身份；Drafts 里的副本也是。
- **换到另一个账户**：`update_draft` 给的是别的账户的身份时，草稿在同一个事务里搬过去
  （`Drafts::move_to`）：改 `account_id` 和 `identity_id`，旧账户 Drafts 里的副本转成一条没有草稿的上传
  （按 Message-ID 找到后删除，和删除草稿一样），还没结算的上传也解除关联（`draft_id` 置空，结算时删掉它做出的
  副本：结算看的是 `draft_uploads` 行此时的 `draft_id`，不是读出时的），清掉 `remote_*` 和
  `upload_error`，由新账户的同步上传到它的 Drafts。旧账户正在进行的一轮上传可能在列出草稿之后、读草稿
  之前遇到搬走，读到的是搬走后的版本：`begin_upload` 只在草稿仍属于目标文件夹的账户时记下上传，否则跳过，
  不会违反 `(folder_id, account_id)` 外键让这一轮失败。两个账户都发 `DraftsChanged`。附件上限不重新检查，
  超出新服务器的 `SIZE` 时由发送报错。**回复不能换账户**（`InvalidInput`）：它回答的那封信在原账户里，
  发出后要在那里加 `\Answered`，而 `reply_to_message_id` 只引用本账户的邮件；判断依据是
  `reply_to_message_id` 或 `in_reply_to` 不为空（原信被删后 In-Reply-To 还在）。转发和新邮件可以换。
  `MailClient::draft_senders` 列出一封草稿可选的身份：回复只有本账户的，其余是所有账户的，按账户排列，
  每个账户的默认身份在前。CLI 的 `--from` 仍只在草稿的账户里按地址找身份。
- **按地址找身份**只有一处：`Identities::by_email`（`MailClient::identity_by_address`），不分大小写；
  几个身份地址相同（经不同服务器）时默认的优先，其次最早添加的。回复和 CLI 的 `--from` 都用它。
- **回复**用原信寄给的那个地址：在原信的 To、Cc 里按顺序找第一个是账户身份的地址；回复自己发出的信
  则用它的发件人。地址对应的身份按上一条找；都不是时用默认身份。账户的地址本身也算
  "自己"（没有身份再用它时也是），不会被加进回复全部的收件人。
- **outbox** 入队时从草稿行复制 `identity_id`（必填，与检查 `revision` 在同一事务）。一轮发送按 SMTP 登录
  分组（见[重试](#重试)），连接失败只推迟经这条登录的条目，退避和账户状态仍按身份（`AccountStatus::send`，
  每项是身份、它的地址和问题，按身份排列）。`FlushReport::errors` 也按身份记下这一轮各台服务器连不上的原因。
- **删除身份**：默认身份拒绝（`Conflict`，先把别的设为默认）；outbox 里还有经它发的条目时也拒绝，否则
  那些信就没有服务器了。用它写的草稿在同一事务里改到默认身份（`revision` 加一，重新上传副本），再删行
  （`RETURNING secret_key`）、删它自己的密钥（如果有），并清掉内存里它的发送状态。删除账户时身份随之
  级联删除，密钥见上。
- `add_identity` 带 `Verify::Login` 时先连它的 SMTP 并登录，失败什么都不存（两个前端都这样用）；
  `Verify::Skip` 不连服务器（CLI 的 `--no-verify`）。

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
  （`Engine::draft_edited`，按草稿），重启后没有记录的草稿直接上传。记录只在后台同步的一步**开始时**
  清掉已到期的（`forget_settled_drafts`，这一步会上传它们）；在一步进行中才到期的留到这一步之后，
  使下一步立即开始，否则它要等到下一次完整同步。用户要求的上传
  （`replay_pending`、`sync_account`）不等，CLI 的行为不变；`sync_now` 由后台同步来做，同样等
  （见[同步 / 后台同步](sync.md#后台同步与-idle)）。
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
  成功后清除。在发出邮件之前就被拒绝（Dovecot 无法写入时不回 `+`，见 [IMAP](protocol.md#连接层)）
  同样是拒绝，连接照常可用。答复是 OK 但有一行解析不了（`Incomplete`）不算拒绝：邮件已经存下，按 Message-ID
  结算。构建失败（附件的 blob 不见了）同样记录，不挡住其他草稿和操作的回放。
- 账户没有 Drafts 文件夹（或还没同步过、不知道有）时草稿只在本地，有了以后由同步上传。
- 只单向上传：Drafts 里其他客户端写的草稿是普通邮件，不变成本地草稿；我们的副本在本地也作为 Drafts
  文件夹里的邮件出现（同步拉下来）。

## Outbox 快照

`send_draft` 把草稿冻结成 outbox 的一行：

- 完整的 MIME 原文此时构建（mail-builder：7-bit、CRLF、RFC 2047 头、RFC 2231 文件名；
  Message-ID 为 `<uuid@发件人域名>`，不泄露主机名；In-Reply-To / References 里的 msg-id 再按收信时
  的规则检查一遍，不合规的丢掉；地址和本信的 Message-ID 丢不得，放不进 `<>`（裸 CR 等）就直接报错，
  不靠调用方先校验）并存成 blob，
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
  每批邮件都要提交，每批都等 fsync 会拖慢同步，排在后面的本地操作也要跟着等。

## SMTP 客户端

`catmail-proto/src/smtp/`，自己实现而不用 lettre：状态机要区分"结束标记发出之前 / 之后"的失败，
lettre 的 `send` 不暴露这一点；自己实现还能共用 `TlsConfig`、超时和 STARTTLS 的处理。一次事务拆成
`begin`（MAIL FROM、逐个 RCPT TO、DATA）、`write_message`（点填充后的正文，不含结束标记）、
`end_message`（结束标记并等答复，RFC 5321 允许服务器想 10 分钟），调用方在后两步之间提交 `unknown`。

- 任何一个收件人被拒绝就 RSET 整个事务，错误里带上那个地址：不会一部分人收到、一部分人没收到。
- 正文里孤立的 CR 或 LF 一律改成 CRLF 再做点填充，没有服务器能在别处看到结束标记（SMTP smuggling）。
- EHLO 用 `[127.0.0.1]`：地址字面量合法，又不暴露主机名和内网地址。
- 服务器声明 SIZE 时，超出的邮件直接失败；有 SIZE 时在 MAIL FROM 带上大小。连上时记下 SIZE，用来限制
  草稿的附件（见[草稿](#草稿)）。
- 服务器不提供 AUTH 时不登录（本地中继）；STARTTLS 配置了就必须成功。
- EHLO 回 4xx 是暂时失败；5xx 才算不支持 ESMTP。
- 结束标记之后：2xx 都算接受；4xx / 5xx 是拒绝，服务器已丢弃邮件；其他答复按结果未知处理。

## 重试

- **按登录分组。** 一轮把排队的信按发件身份的 SMTP 登录分组（`connection::smtp_logins`）：服务器、用户名
  和密码都相同的身份（例如同一个邮箱服务商里设置的几个别名）共用一条连接，keyring 条目每个只读一次。密码
  不同就分开：其中一个过时了，用它登录失败不该挡住另一个。共用的连接登录失败时，每个身份都记上这次失败，
  这一组推迟的间隔按组里失败次数最多的身份算。
- **各组并发。** 每组在这一轮的任务里各自一个 future（`join_all`），先连接、再按入队顺序逐封发送，发完
  各自 QUIT。一台服务器不回答时（每次读写最多等 60 秒）只拖住经它的信。各组的 `unknown` 照旧在结束标记
  之前用 `write_durable` 提交；存 Sent 之前的重试、`FlushReport` 的合并都等各组结束（条目按 id 排序）。
  一组中途出错时其他组照样做完，再返回第一个错误。不用 `spawn`：账户被删除时整轮随 future 一起被丢弃，
  发送锁释放后不会还有一组在发。
- 每组先连上并登录 SMTP 服务器，再开始第一封（`begin_send`），同一组的多封邮件复用这条连接。连接时
  的失败（连不上、TLS、登录被拒绝、读不到密码、服务器缺少所需能力）属于发件人（账户或身份）而不是某一封信：这一轮
  所有排队的都推迟到同一时刻，不计尝试次数，`last_error` 写上原因；间隔按**发件人**连续失败的次数从
  30 秒起翻倍、最多 30 分钟（失败次数记在账户状态里，见下）；睡眠醒来后先从 2 秒起快速重试（见
  [后台 / 系统睡眠与唤醒](background.md#系统睡眠与唤醒)）。否则密码错误时每轮会让一封变成 `failed`，
  打开后台发送后整个发件箱就在无人值守时逐封失败了。
- 开始之后的 4xx、断线、超时是这封信的暂时失败，按它自己的尝试次数同样退避；5xx、服务器缺少所需能力
  （如 SIZE 超限）、邮件过大是永久失败。
- **账户状态。** 连接成功或失败都记进引擎的每账户状态（`MailClient::account_status` 的 `send`），变化
  时发 `AccountStatusChanged`，前端据此提示"密码被拒绝"等，不必从某一封信的 `last_error` 去猜。
  发信问题自带所属身份的地址，几台服务器都出问题时前端全部列出，不必再去身份列表里查。
- **flush 的结果。** `flush_outbox` 返回 `FlushReport`：处理过的条目（推迟的也算）按最终状态列出，
  连接失败时 `errors` 按身份记下各自的原因（带 `transient`）：一个身份的密码被拒绝、另一个的服务器
  暂时连不上时，两者都在，不会互相覆盖。调用方据此区分"稍后重试即可"和"要先改账户"，
  不必再读账户状态，那可能是别的发送留下的。连接失败不作为 `Err` 返回：那样调用方就丢了被推迟条目的
  结果；`Err` 只用于一轮中途出错。
- 一轮中途出错（例如写库失败）时，已经做完的也照样发 `OutboxChanged`。
- **系统时间往回调。** `next_attempt_at` 是墙上时钟，要跨重启保存，不能像同步那样改用内存里的单调时钟。
  往回调之后，之前推迟的条目的到期时间留在"未来"，本该很快重试的要等到时钟追上来。任何退避都不超过
  `MAX_BACKOFF`，晚于 `now + MAX_BACKOFF` 的时间只能是这样留下的：后台发件每一轮先把它们改成现在
  （`Outbox::bring_forward`），再找到期的条目，下一轮的等待也就按改过的时间算。

## 保存到 Sent

被接受后在同一轮里立即 APPEND（带 `\Seen`），失败的留到之后几轮重试。目标优先用本地已知的 Sent
角色文件夹；账户还没同步过时用 LIST 按同步相同的角色规则现找；没有 Sent 就跳过。APPEND 走按需
下载的连接池。

APPEND 的答复也可能丢失，所以尝试次数在每次尝试**之前**加一：第一次直接 APPEND，第二次起（包括同一
次尝试里换新连接重来）先 EXAMINE Sent、按 Message-ID `UID SEARCH HEADER`，已经在了就不再 APPEND。

有的服务器自己把经它的 SMTP 发出的邮件存进已发送（Gmail 会），再 APPEND 会多一份。所以身份记下"服务器会
自动存已发送"（`identities.saves_sent`，`Identity::server_saves_sent`），是的跳过这一步：

- **用户说了算**：GUI 的身份设置和 CLI 的 `identity saves-sent <id> yes|no|auto` 可以改它
  （`set_identity_saves_sent`），改过之后不再猜；`auto` 放回去重新猜。
- **没说过时猜**：服务器是 Gmail 的（`X-GM-EXT-1`），而身份经同一个登录、同一个域（`imap.gmail.com` /
  `smtp.gmail.com`）的 SMTP 发信。身份经别的服务器发出的 Gmail 看不到，照常保存；别的服务商一概当作不存。
  看的是服务器是不是 Gmail，不是账户模式："所有邮件"在 IMAP 中隐藏的 Gmail 账户也自动存已发送。
- **猜测不存下来**，每次保存时现猜：存一个默认值就得在添加身份、第一次认出 Gmail 时各写一次，还要另记
  "用户改过"，而结果和现猜一样。猜的依据是记下的 `accounts.gmail_server`，每次连上 IMAP 服务器时记下
  （`connection::connect`；带登录检查添加账户时由那次登录记下），界面显示和保存用的是同一个值。不检查登录
  添加、还没连上过的账户记下的是 false：保存要连上服务器，拿到会话之后按它（也就是连接时记下的）再猜一次，
  第一次同步之前从 Gmail 发出的也不会多存一份。

见[位置与标签](labels.md#草稿与已发送)。

## 谁来发

`MailClientBuilder::send_in_background(true)` 时引擎起一个后台任务，按 `next_attempt_at` 和入队、
重试的通知处理到期的条目（GUI 用）；它只持有 `Weak<Engine>`，不会让数据目录锁一直被占着，并且
随引擎的关闭信号结束，借用的 runtime 上也不会在客户端没了之后再睡上最长 30 分钟。CLI 不开
后台发送，免得一个无关的命令在背后发信：`catmail send` 入队后立即 `flush_outbox`，失败的留给
`catmail outbox flush`。每账户一把发送锁（`Engine::send_permit`），后台发送和手动 flush 不会同时
处理同一条。

发送相关的事件：`DraftsChanged`、`OutboxChanged`；服务器接受一封邮件时另有 `MessageSent`。
