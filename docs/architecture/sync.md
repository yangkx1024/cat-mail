# 同步

[← 架构](../architecture.md)

`MailClient::sync_account`：一条连接，逐个文件夹 EXAMINE（只读，不改 `\Recent`）。同一账户的
同步与操作回放由每账户的同步锁串行化。代码在 `crates/catmail-core/src/sync.rs`、`engine.rs`。

## 流程

1. **对账文件夹。** `LIST "" "*"`：新增、更新属性、删除服务器上已不存在的文件夹（连同邮件）。
   - RFC 3501 §6.3.8 规定只要用户有 INBOX 就要列出它。LIST 结果里没有 INBOX 时，
     `Session::list` 用 EXAMINE INBOX 问一次：成功说明响应被截断或损坏，返回 `Incomplete`，
     整次同步报错、什么也不删；失败说明确实没有 INBOX，结果照常使用。
   - 角色依次取：名为 INBOX → SPECIAL-USE 属性 → 常见名称（`Sent Items`、`已发送`、`垃圾邮件`
     等）。按名称猜测只用于没有任何文件夹通过 SPECIAL-USE 声明的角色，每个角色只猜一个。
   - `\Noselect` 的层级节点也保存（`selectable = 0`），但不同步。可选中的邮箱变成层级节点时
     （删除有子文件夹的邮箱会清空它并保留名字），按"这一代 UID 结束"处理：删除邮件、清空
     UIDVALIDITY 与游标、离线操作标为 `conflict`，只保留这一行。
2. **上传改过的草稿、回放操作队列**，在同步任何文件夹之前，让同步看到改动之后的服务器（见
   [草稿同步](sending.md#同步到服务器)、[操作队列](op-queue.md)）。
3. **EXAMINE 并比对 UIDVALIDITY**（服务器支持时带 `CONDSTORE`）。变化或第一次同步时，在同一个
   事务里删除该文件夹的所有邮件、写入新的 UIDVALIDITY、清空游标，把旧代际的 `pending_ops` 标为
   `conflict`，清掉按旧 UID 指向它的草稿远端副本（改为按 Message-ID 重新找，见
   [草稿同步](sending.md#同步到服务器)）。
4. **新邮件与已删除邮件。**
   - 游标里的 UIDNEXT 与服务器相同、本地条数等于 EXISTS：没有变化，不发 SEARCH。
   - 否则 `UID SEARCH UID <游标>:*` 只取游标之后的 UID；"本地条数 + 新邮件数 = EXISTS" 说明
     没有删除，不必传输大文件夹的完整 UID 列表。
   - 仍对不上（有删除，或没有游标）时才 `UID SEARCH ALL`，与本地 UID 列表求差集。
5. **已有邮件的 flags。** 有 CONDSTORE 时一条 `UID FETCH 1:<本地最大 UID> (FLAGS) (CHANGEDSINCE <游标>)`，
   HIGHESTMODSEQ 没变则整个跳过。没有 CONDSTORE（或 NOMODSEQ）时只能全量拉：严格命令的答复整个
   缓冲在内存里，所以把本地 UID 列表每 5000 个分成一个窗口（窗口取其首尾 UID 为范围，UID 再稀疏
   也不会发出空命令）、一条 FETCH、一个事务，内存有界，断线时已写入的窗口保留。后台同步两次完整
   同步之间的 INBOX 步骤可以不拉（见[后台同步](#后台同步与-idle)）；这样的同步照样写游标，但不算
   完成的同步、不更新 `last_synced_at`。有 CONDSTORE 时不能跳过：写入的 HIGHESTMODSEQ 会声称
   这些变化已经看过。有未回放的标记操作时，把它们叠加在服务器的 flags 上（`Messages::update_flags`）；
   只有 MODSEQ 变化不算 flags 更新。
6. **拉取新邮件。** 从新到旧，每批 200 封
   `UID FETCH (UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (…)])`，
   每批一个事务，提交后发 `MessagesChanged`，界面先看到最新的邮件。
7. **写游标。** 文件夹全部完成后才写（UIDNEXT、HIGHESTMODSEQ，取自 EXAMINE 时刻）。某批拿回的
   邮件比请求的少（SEARCH 之后被删，或响应无法解析）时不写，下次完整求差集。

## 游标与断点续传

游标只代表"一次完整的同步"，不随批次推进：从新到旧拉取时 UIDNEXT 没法按批推进，而已经入库的
邮件行本身就是进度。中断后下一次同步重新求差集，只拉缺的那些（`upsert` 遇到已有 UID 只刷新
flags，重复的批次不产生重复行）。HIGHESTMODSEQ 用 EXAMINE 时刻的值，之后的变化 modseq 更大，
下次 `CHANGEDSINCE` 一定能拿到。

## 容错

- 服务器对某个文件夹回 NO/BAD（例如 LIST 之后被删除），或 EXAMINE 缺少 UIDVALIDITY / EXISTS，
  只记在该文件夹的报告里，其余文件夹照常同步。断线、超时、BYE 让整个同步失败，已提交的进度
  保留。
- 重复或乱序的 FETCH 响应按 UID 去重；不带 UID 的主动 FETCH 和不在请求集合里的 UID 忽略；同一封
  邮件的属性分在几条 FETCH 响应里时按 UID 合并（RFC 9051 允许）。
- 无法解析的响应行默认让命令返回 `Incomplete`（见[连接层](protocol.md#连接层)）：LIST、SEARCH、
  EXAMINE、flags 的结果缺一行，就会被当成文件夹被删、邮件被删或 flags 没变。
- 只有拉邮件头用 `execute_lossy`。一条 FETCH 解析失败通常是 imap-codec 不接受服务器的
  BODYSTRUCTURE，每次都会重现，所以缺失的 UID 立即再取一次、不带 BODYSTRUCTURE
  （`has_attachments` 先记为否，下载正文后重算）。仍然缺失的不出现在结果里，那一批算不完整、
  不推进游标。
- 认证失败、服务器缺少设置要求的能力（如 STARTTLS）在 core 里转成 `Error::AuthFailed` /
  `Error::Unsupported`，错误信息带给用户的建议，前端直接显示。

## 打开的文件夹

`MailClient::sync_folder(folder, max_age)` 只同步一个文件夹：持有账户的同步锁，在连接池的连接上先回放
操作队列（以及停止编辑够久的草稿，与后台同步相同），再同步这个文件夹，结果是它的 `FolderSyncReport`；服务器
拒绝这个文件夹时记在报告里，不算失败。它在 `max_age` 内完成过同步（`last_synced_at`）就什么也不做，返回
`None`；这个检查在拿到锁之后，所以等着的那次完整同步刚同步过它时不会再来一遍。只有真的连了服务器的
同步才计入账户状态：跳过的那次什么也没说明，不能清掉侧栏里的连接问题，更不能清掉"登录被拒"（那会唤醒
后台同步、绕过它的重试间隔）。GUI 在用户打开文件夹时
调用（见 [GUI](gui.md)）：服务器无法告诉我们 INBOX 以外的变化（见下一节），用户正在看的文件夹不该等到下一次
完整同步。

## 后台同步与 IDLE

`MailClientBuilder::sync_in_background(Some(SyncSchedule))` 时（GUI、`catmail watch`），引擎为每个
账户起一个后台任务（`crates/catmail-core/src/watch.rs`），它只有一条连接。间隔都在 `SyncSchedule`
里，括号里是默认值。

- **同步什么。** 每隔 `full_sync`（15 分钟）同步全部文件夹（`sync::run`，与 `sync_account` 相同；
  任何一次完整同步都算，包括用户手动的），其间只盯 INBOX：服务器支持 IDLE 时在 INBOX 上 IDLE，
  等服务器报告变化；不支持或拒绝 IDLE 时每隔 `poll`（2 分钟）检查一次。每一步最后同步 INBOX、让它
  保持选中，然后 IDLE。只盯 INBOX，是因为一条连接只能 IDLE 一个文件夹，而 NOTIFY（RFC 5465）支持的
  服务器太少；其他文件夹靠定时的完整同步，和用户打开它时的同步（见[打开的文件夹](#打开的文件夹)）。
- **本地改动立即回放。** 标记、移动、删除以及回复后的 `\Answered` 入队后唤醒任务
  （`Engine::wake`；任务正忙时唤醒留到它下次等待），它结束 IDLE、回放队列、再同步 INBOX。改过的
  草稿例外：要在 `draft_upload_delay` 内没再改才上传（见[草稿同步](sending.md#同步到服务器)），
  IDLE 的时限因此取下一次完整同步和最早一份被留下的草稿到期中较早的一个。
- **锁。** 每一步持有账户的同步锁，与手动同步、`replay_pending` 串行；等待（IDLE、轮询和重试的间隔）
  时不持有，所以用户要求的同步不必等 IDLE 结束，它用自己的连接。连接在拿锁之前建好：服务器连不上时
  不挡住手动同步。
- **IDLE 之前的变化。** 服务器只把一次变化告诉一次，可能在同步 INBOX 时顺带在别的命令的答复里说了
  （例如 FETCH 中间的 `* 12 EXISTS`）。连接层记下每个答复里的这类报告（见[连接 / IDLE](protocol.md#idle)），
  `Session::idle` 发现打开 INBOX 以来已经有过变化就立即返回，任务再同步一遍。
- **重新 IDLE。** 每次 IDLE 最长 `idle`（10 分钟，RFC 2177 要求不超过 29 分钟）。期间什么也没发生
  就直接再 IDLE，不同步：IDLE 与 DONE 的往返已经说明连接还活着，无声断开的连接（NAT 忘了它）在那时
  超时。没有 CONDSTORE 的服务器上同步 INBOX 要拉全部 flags，不该每 10 分钟做一次。
- **INBOX 的 flags。** 没有 CONDSTORE 时，两次完整同步之间的 INBOX 步骤只在 IDLE 报告了 flags 变化
  （IDLE 期间的 FETCH，或无法解析的行，`Session::flags_changed`）时才拉全部 flags；新邮件、删除、
  本地改动唤醒的步骤和轮询都不拉，否则五万封的收件箱每来一封新邮件就是一次全量 flags 遍历。轮询时
  其他客户端改的 flags 要等下一次完整同步，或用户打开 INBOX 时的同步（这些步骤不更新
  `last_synced_at`，所以不会让它显得刚同步过）。刚做完完整同步的那一步也不再拉一遍。很多家用 NAT 五分钟
  左右就忘掉安静的连接，所以连接都开着 TCP keepalive（见[连接层](protocol.md#连接层)），几分钟内发现，
  不必等到这次 IDLE 到期。
- **不正常的 IDLE。** 服务器自己用 OK 结束 IDLE 算安静地结束，照常再 IDLE；但连续三次一开始就这样
  结束（不到一半时长、也不到一分钟）说明 IDLE 对它没用，改为轮询，否则就是一个接一个的往返。反过来，
  IDLE 一开始就报告变化（无法解析的行也算变化）连续超过十次时，每次同步 INBOX 之间等一会儿（从
  `retry` 起翻倍，最长 `poll`），否则会不停地同步 INBOX；真正繁忙的收件箱这时退化为短间隔的轮询。
- **失败。** IDLE 中连接断开（服务器或 NAT 关掉了它）立即重连；但 IDLE 开始后不到一分钟就断开、而且
  连续发生（例如代理每次都关掉 IDLE 的连接）时，从第二次起按下面的间隔退避，否则会不停地登录、同步；一步开始时复用的连接已断开，也立即换
  新连接重来一次（与连接池同一条规则，`connection::on_session`）。其他失败（连不上等）从 `retry`
  （30 秒）起翻倍等待，最长 `full_sync`，有本地改动入队时立即再试。**登录被拒绝**例外：每隔
  `full_sync` 才再试一次，本地改动也不提前唤醒它（这时 `Engine::wake` 什么也不做），因为每次重试都是
  又一次失败的登录，服务器（AD、Exchange 等）累计几次就会锁定账户；用户要求的同步或回放成功（例如改好
  了密码）清除这个状态时立即唤醒它继续。
- **账户状态。** 每一步的结果、用户要求的 `sync_account` / `replay_pending`（真的连了服务器时）都记进
  引擎里每账户的状态（`Engine::report`，`MailClient::account_status` 的 `sync`）：连续失败时记下最近的
  错误、是否登录被拒绝、从什么时候开始、失败了几次，成功就清空。出现、清除或错误变了时发
  `AccountStatusChanged`（不是每次失败都发），前端据此显示（`catmail watch` 打印一行；GUI 在 M5）。
  状态只在内存里，引擎新打开时什么都不知道。
- **生命周期。** 引擎打开时为已有账户、`add_account` 时为新账户启动任务（账户 id 不复用，每个账户只
  启动一次）。账户删除时任务结束，IDLE 中也立即结束，删除不必等它。任务只持有引擎的弱引用；等待时
  用账户的取消信号，它在账户删除或引擎不在时（发送端随引擎 drop）都会触发：最后一个 `MailClient`
  drop 后任务结束并关闭连接，不会让引擎和数据目录锁多活一个 IDLE 周期。
- **找 INBOX** 按角色（对账时按名称不分大小写认定），只读这一个文件夹的同步状态。

## 文件夹名

不支持 UTF8=ACCEPT 的服务器用修改版 UTF-7 传输非 ASCII 名称（`&XfJT0ZAB-` = `已发送`）。
`folders.remote_name` 原样保存服务器发来的名称，命令里只用它；`name` 是解码后的显示名，解码
失败时等于原名。

## 邮件头

只拉 `HEADER.FIELDS (DATE FROM TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES)`，用
`mail-parser`（`full_encoding`）解析，RFC 2047 编码字和 GBK、GB2312、Big5 等旧字符集都转成
UTF-8。没有（或无法解析）Date 头时用 INTERNALDATE 排序。不合法的 msg-id（mail-parser 会保留
其中的裸 CR，回复时会注入邮件头）在这里就丢掉。`has_attachments` 先由 BODYSTRUCTURE 粗略判断
（`MimeNode::has_attachments`），下载正文后按完整 MIME 树重算。
