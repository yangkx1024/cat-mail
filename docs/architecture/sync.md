# 同步

[← 架构](../architecture.md)

`MailClient::sync_account`：一条连接，逐个文件夹 EXAMINE（只读，不改 `\Recent`）。同一账户的
同步与操作回放由每账户的同步锁串行化（见[锁](#锁)）。代码在 `crates/catmail-core/src/sync.rs`、`engine.rs`。

同步的是**位置**（`folders.kind = 'location'`）：普通账户上是所有可选中的邮箱。Gmail 账户上只有"所有邮件"、
已删除、垃圾邮件三个，作为一个单元同步，其余邮箱是标签，随邮件的 `X-GM-LABELS` 存下、不 EXAMINE，见
[位置与标签 / 同步](labels.md#同步)。下面说的流程两者相同，差别都在那一节。

## 流程

1. **对账文件夹。** `LIST "" "*"`：新增、更新属性、删除服务器上已不存在的文件夹（连同邮件）。
   按能力和 LIST 定下账户的模式（普通 / Gmail）和每个邮箱是位置还是标签；模式变了，所有位置像
   UIDVALIDITY 变化那样重来。
   - RFC 3501 §6.3.8 规定只要用户有 INBOX 就要列出它。LIST 结果里没有 INBOX 时，
     `Session::list` 用 EXAMINE INBOX 问一次：成功说明响应被截断或损坏，返回 `Incomplete`，
     整次同步报错、什么也不删；失败说明确实没有 INBOX，结果照常使用。
   - 角色依次取：名为 INBOX → SPECIAL-USE 属性 → 常见名称（`Sent Items`、`已发送`、`垃圾邮件`
     等）。按名称猜测只用于没有任何文件夹通过 SPECIAL-USE 声明的角色，每个角色只猜一个；Gmail 上
     不猜。
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
   同步之间的 INBOX 步骤只拉 IDLE 报告变化的那几封（见[后台同步](#后台同步与-idle)）；这样的同步照样
   写游标，但不算完成的同步、不更新 `last_synced_at`。有 CONDSTORE 时不能跳过：写入的 HIGHESTMODSEQ 会声称
   这些变化已经看过。有未回放的标记操作时，把它们叠加在服务器的 flags 上（`Messages::update_flags`）；
   只有 MODSEQ 变化不算 flags 更新。
6. **拉取新邮件。** 从新到旧，每批 200 封（`EngineTuning::fetch_batch`）
   `UID FETCH (UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (…)])`，
   每批一个事务，提交后发 `MessagesChanged`，界面先看到最新的邮件。同一 UIDVALIDITY 下 UID 不小于游标
   UIDNEXT、进了 INBOX 的未读邮件另发 `NewMail`（见[后台运行 / 新邮件](background.md#新邮件)）。
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
- 只有拉邮件头用 `execute_lossy`。一条 FETCH 解析失败通常是解析器不接受服务器的
  BODYSTRUCTURE，每次都会重现，所以缺失的 UID 立即再取一次、不带 BODYSTRUCTURE
  （`has_attachments` 先记为否，下载正文后重算）。仍然缺失的不出现在结果里，那一批算不完整、
  不推进游标。
- 认证失败、服务器缺少设置要求的能力（如 STARTTLS）在 core 里转成 `Error::AuthFailed` /
  `Error::Unsupported`，错误信息带给用户的建议，前端直接显示。
- 一个文件夹的命令被拒绝（`Error::is_refusal`，连接仍可用）只记进它的 `FolderSyncReport.error`，
  其余文件夹照常同步；连接断了才让整次同步失败。

## 锁

每个账户一把同步锁（`Engine::sync_permit`），同步、操作回放、打开文件夹时的同步和后台同步的每一步都要
拿到它。完整同步（`sync::run`）不是整次持有，而是对账文件夹并回放操作时持有一次，之后每个文件夹各拿
一次：锁是公平的（tokio `Mutex` 先来先得），等着的同步在两个文件夹之间插进来，最多等完整同步做完手上
那一个文件夹。否则首次同步一个大账户要几分钟，这期间用户点开的文件夹一直空着、也没有提示。

- **插进来的同步做完的文件夹不再同步。** 引擎在内存里记着每个文件夹最近一次完整同步（邮件和全部
  flags 都拉过）完成的单调时刻（`Engine::folder_synced`，`Instant`），完整同步跳过晚于它开始的文件夹，
  报告里只有回放的计数。没有 CONDSTORE 的服务器上，再同步一遍就是把这个文件夹的 flags 全量拉一遍。
  - "开始"取第一次拿到锁的时刻，而不是调用的时刻：拿锁时没有别的同步在进行，之后完成的同步都是之后才
    EXAMINE 的；调用时正在进行的那次可能在调用之前就 EXAMINE 了，跳过就会漏掉其间到达的邮件。
  - 不用 `last_synced_at`：它是墙上时钟，系统时间往回调之后，留在"未来"的时间会让此后每次完整同步都
    跳过这个文件夹，直到时钟追上来；它又是秒精度，同一秒里、本次开始之前的同步也会被算成"之后"。
- **每个文件夹的状态在拿到锁之后重新读。** 开头读到的 UIDVALIDITY 和游标可能已经过时：插进来的同步
  可能开始了新的 UIDVALIDITY，另一次完整同步对账时可能删掉了这个文件夹（那就跳过）。
- 两次完整同步（手动的与后台的）同样按文件夹交替，各自跳过对方在自己开始之后做完的，合起来大致只同步
  每个文件夹一遍。`sync_account` 在拿锁之前连接服务器，账户被删除时立即返回 `Error::Cancelled`。
- 只同步一个文件夹（打开文件夹、后台同步的 INBOX）和只回放操作的，整次持有锁：它们本来就短。

## 打开的文件夹

`MailClient::sync_folder(folder, max_age)` 只同步一个文件夹（Gmail 上是三个位置，不论打开的是哪个）：持有账户的同步锁，在连接池的连接上先回放
操作队列（以及停止编辑够久的草稿，与后台同步相同），再同步这个文件夹，结果是它的 `FolderSyncReport`；服务器
拒绝这个文件夹时记在报告里，不算失败。它在 `max_age` 内完成过同步（`Engine::folder_synced`，与[锁](#锁)
一节的完整同步用同一个单调时钟）就什么也不做，返回 `None`；这个检查在拿到锁之后，所以正在进行的完整同步
刚同步过它时不会再来一遍。不用 `last_synced_at`，理由与那里相同：系统时间往回调之后，留在"未来"的时间
会让每次打开都被当成刚同步过，直到时钟追上来。代价是这个记录只在内存里：重启之后每个文件夹第一次打开
都会同步一次，直到启动时的完整同步做完它。正在进行的完整同步只让它
等完手上那个文件夹（见[锁](#锁)）。只有真的连了服务器的
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
- **锁。** 每一步持有账户的同步锁（完整同步按文件夹拿，见[锁](#锁)），与手动同步、`replay_pending` 串行；等待（IDLE、轮询和重试的间隔）
  时不持有，所以另开连接的同步（`sync_folder`、`sync_account`、`replay_pending`）不必等 IDLE 结束。
  连接在拿锁之前建好：服务器连不上时不挡住它们。
- **现在同步**（`MailClient::sync_now`，GUI 的 Sync 按钮、Ctrl R、托盘的"Sync now"）由后台任务来做，
  不另开连接：每个账户记下要求的时刻（`Engine::ask_to_sync`），开始于它之前的完整同步不再算数（与醒来的
  时刻同一套比较，`Cutoff`），再像本地改动一样唤醒任务，它的下一步就是一次完整同步。正在进行的那次
  完整同步可能在要求之前就做完了某些文件夹，所以做完后再来一遍。与 `resume` 不同，它不改醒来的时刻，
  连接照常用：IDLE 中的连接发 DONE 结束，万一它已经无声断开，要等 DONE 的答复超时（`IO_TIMEOUT`）或
  keepalive 发现才重连。正在同步（`syncing`，见下）的账户不再要求，进行中或待做的那次就是答复；登录被
  拒绝的账户不理会（与 `wake`、`resume` 相同）。规则只在引擎里（`Engine::begin_syncing`），前端不另加。
  - **进度和结果**都在账户状态里：要求时 `syncing` 置位，开始于要求之后的那一步（必定是完整同步）
    结束时撤下，不论成败，失败记进 `sync`，都只发一次 `AccountStatusChanged`。任何一次同步发现登录被拒
    时也撤下，不论它开始于要求之前还是之后：之后后台同步按 `full_sync` 等着，要求不会被答复，否则侧栏
    要显示"Syncing…"十五分钟，盖住"无法登录"。任务的第一步（引擎打开或
    添加账户之后的首次完整同步）同样显示为 `syncing`：任务 spawn 之前就像被要求过一样置位
    （`Engine::begin_syncing`，只是不唤醒），添加账户后前端马上就能显示。此后每 15 分钟的完整同步不算，
    用户没在等它。
  - 前端不再自己逐个账户同步（#152）。代价：文件夹级的失败（服务器拒绝某个文件夹）不再弹通知，与后台
    同步一样只在报告里、记日志；同步也只上传停止编辑够久的草稿（`draft_upload_delay`）。
  - 没有后台同步时 `sync_now` 返回 `Error::NoBackgroundSync`，什么也不置位（没有任务来撤下）。起初它
    在调用里逐个 `sync_account`，但走那条分支的只有关掉后台同步的 GUI 测试：一个方法两套行为（同步完
    才返回、另开连接、上传全部草稿），测试也测不到真实的路径。现在按 Sync 的 GUI 测试用开着后台同步的
    客户端（`TestMail::start_syncing_in_background`）。CLI 的 `catmail sync` 仍直接调 `sync_account`：
    一次性的命令没有后台同步可以催。
  - 报告时带上这次尝试开始的时刻（`Activity::SyncBegun`：后台同步的每一步、`sync_account`），开始于
    要求之后的就撤下 `syncing`。后台同步只同步 INBOX 的步骤也这样报告：要求之后全部文件夹都到期，
    开始于要求之后的那一步一定是完整同步。
- **IDLE 之前的变化。** 服务器只把一次变化告诉一次，可能在同步 INBOX 时顺带在别的命令的答复里说了
  （例如 FETCH 中间的 `* 12 EXISTS`）。连接层记下每个答复里的这类报告（见[连接 / IDLE](protocol.md#idle)），
  `Session::idle` 发现打开 INBOX 以来已经有过变化就立即返回，任务再同步一遍。
- **重新 IDLE。** 每次 IDLE 最长 `idle`（10 分钟，RFC 2177 要求不超过 29 分钟）。期间什么也没发生
  就直接再 IDLE，不同步：IDLE 与 DONE 的往返已经说明连接还活着，无声断开的连接（NAT 忘了它）在那时
  超时。很多家用 NAT 五分钟左右就忘掉安静的连接，所以连接都开着 TCP keepalive（见[连接层](protocol.md#连接层)），
  几分钟内发现，不必等到这次 IDLE 到期。
- **INBOX 的 flags。** 没有 CONDSTORE 时，两次完整同步之间的 INBOX 步骤只拉 IDLE 报告了 flags 变化的
  那几封（`Session::changed_flags`，`UID FETCH <这些 UID> (UID FLAGS)`）；新邮件、删除、本地改动唤醒
  的步骤和轮询都不拉，否则五万封的收件箱每来一封新邮件、每读一封邮件就是一次全量 flags 遍历。说不清
  是哪几封时（IDLE 期间有无法解析的行、报告超过一千封或遇到 VANISHED，见[连接 / IDLE](protocol.md#idle)）
  才按窗口全量拉。报告记到 INBOX 真的同步成功为止，在几次失败的步骤间累积（同步时仍按窗口分批）：这一步
  失败（包括只让 INBOX 失败的非致命错误）时，下一步还会拉。只拉部分 flags 的同步不更新
  `last_synced_at` 和 `Engine::folder_synced`，所以不会让 INBOX 显得刚同步过。IDLE 以外的变化（轮询、IDLE 之间的同步、断线期间）
  其他客户端改的 flags 要等下一次完整同步，或用户打开 INBOX 时的同步。
- **不正常的 IDLE。** 服务器自己用 OK 结束 IDLE 算安静地结束，照常再 IDLE；但连续三次一开始就这样
  结束（不到一半时长、也不到一分钟）说明 IDLE 对它没用，改为轮询，否则就是一个接一个的往返。反过来，
  IDLE 一开始就报告变化（无法解析的行也算变化）连续超过十次时，每次同步 INBOX 之间等一会儿（从
  `retry` 起翻倍，最长 `poll`），否则会不停地同步 INBOX；真正繁忙的收件箱这时退化为短间隔的轮询。
- **失败。** IDLE 中连接断开（服务器或 NAT 关掉了它）立即重连；但 IDLE 开始后不到一分钟就断开、而且
  连续发生（例如代理每次都关掉 IDLE 的连接）时，从第二次起按下面的间隔退避，否则会不停地登录、同步；一步开始时复用的连接已断开，也立即换
  新连接重来一次（与连接池同一条规则，`connection::on_session`）。其他失败（连不上等）从 `retry`
  （30 秒）起翻倍等待，最长 `full_sync`，有本地改动入队时立即再试。**登录被拒绝**例外：每隔
  `full_sync` 才再试一次，本地改动、睡眠唤醒也不提前叫醒它：任务被叫醒时查一下账户状态，登录仍被拒绝
  就接着等（`unless_rejected`），在得知被拒之前就排着的通知也一样，因为每次重试都是
  又一次失败的登录，服务器（AD、Exchange 等）累计几次就会锁定账户；用户要求的同步或回放成功（例如改好
  了密码）清除这个状态时立即唤醒它继续。
- **账户状态。** 每一步的结果、用户要求的 `sync_account` / `replay_pending`（真的连了服务器时）都记进
  引擎里每账户的状态（`Engine::report`，`MailClient::account_status` 的 `sync`；`syncing` 见上面的
  现在同步）：连续失败时记下最近的
  错误（`ReportedError`，带 `ErrorKind`；登录被拒绝是 `Auth`）、从什么时候开始、失败了几次，成功就
  清空。出现、清除或错误变了时发 `AccountStatusChanged`（不是每次失败都发），前端据此显示
  （`catmail watch` 打印一行；GUI 侧栏按种类显示"无法登录 / 连不上 / 无法同步"）。
  状态只在内存里，引擎新打开时什么都不知道。
- **生命周期。** 引擎打开时为已有账户、`add_account` 时为新账户启动任务（账户 id 不复用，每个账户只
  启动一次）。任务的第一条连接先从连接池里拿：`Verify::Login` 添加时检查密码登录好的
  IMAP 连接（什么都没选中）放进池里，用户等收件箱时不必再握手、登录一遍；没有后台同步时留给下一个
  按需操作。检查或插入失败时 LOGOUT，连接不留。账户删除时任务结束，IDLE 中也立即结束，删除不必等它。任务只持有引擎的弱引用；等待时
  用账户的取消信号，它在账户删除或引擎不在时（发送端随引擎 drop）都会触发：最后一个 `MailClient`
  drop 后任务结束并关闭连接，不会让引擎和数据目录锁多活一个 IDLE 周期。
- **睡眠唤醒。** `MailClient::resume` 记下醒来的时刻：睡前建立的连接、睡前开始的同步不再算数，任务丢掉
  这样的连接（IDLE 中不发 DONE）、做一次完整同步，登录被拒绝的除外。见
  [后台运行 / 系统睡眠与唤醒](background.md#系统睡眠与唤醒)。
- **找 INBOX** 按角色（对账时按名称不分大小写认定），只读这一个文件夹的同步状态。
- **Gmail** 上 INBOX 是标签：被唤醒（或轮询）时同步的是三个位置，再 EXAMINE INBOX 接着 IDLE，见
  [位置与标签 / 同步](labels.md#同步)。

## 文件夹名

不支持 UTF8=ACCEPT 的服务器用修改版 UTF-7 传输非 ASCII 名称（`&XfJT0ZAB-` = `已发送`）。
`folders.remote_name` 原样保存服务器发来的名称，命令里只用它；`name` 是解码后的显示名，解码
失败时等于原名。

## 邮件头

只拉 `HEADER.FIELDS (DATE FROM TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES)`，用
`mail-parser`（`full_encoding`）解析，RFC 2047 编码字和 GBK、GB2312、Big5 等旧字符集都转成
UTF-8。直接用原始 8 位字节写的主题、显示名（不按 RFC 2047 编码）按 BODYSTRUCTURE 里文本部分的
`charset` 转码（`MimeNode::text_charsets`，规则见[原始字节的邮件头](bodies.md#原始字节的邮件头)），
没有 BODYSTRUCTURE 或没有线索时保持 mail-parser 的有损结果。没有（或无法解析）Date 头时用 INTERNALDATE 排序。不合法的 msg-id（mail-parser 会保留
其中的裸 CR，回复时会注入邮件头）在这里就丢掉。`has_attachments` 先由 BODYSTRUCTURE 粗略判断
（`MimeNode::has_attachments`），下载正文后按完整 MIME 树重算。
