# 修复 M4 之后的遗留问题：草稿 NUL、账户级失败、发信取消、后台 GC 与操作队列

- 日期：2026-10-20
- 类型：修复

## 背景

M4 合并后、开始 M5 之前，按 AGENTS.md 的规则分四个方向审查了一遍代码（操作队列、IDLE 与后台同步、
草稿同步、全局规则），规则本身没有被违反，但找到一批会在实际使用中出问题的地方，其中几条是 M5 的前提
（GUI 要打开后台发送，并显示后台出的错）：

- **草稿里有 NUL 时整个账户的同步卡死。** 草稿只检查地址；mail-builder 不认为 NUL 需要编码，纯 ASCII
  的正文原样以 7bit 写出；imap-types 的 literal 不允许 NUL，`Session::append` 组装命令时返回致命的
  `Protocol` 错误；上传在同步任何文件夹之前，所以每次同步都在这里失败。回复一封文本带 QP `=00` 的来信
  就能触发（外部可控）。
- **密码错误时发件箱被逐封判失败。** 连接失败记在队首那封上，认证失败是永久错误、`next_attempt_at`
  清空，其余的推迟到 `now + 30 s`，于是每 30 秒一封变成 `failed`。
- **登录被拒绝后，每个本地改动都会让后台同步再登录一次**：退避等待被唤醒打断。连续打开几封信（M5 会
  标已读）就是几次失败的登录，足以让 AD / O365 锁定账户。
- **后台同步和发信的错误只记日志**，GUI 里看不到。
- **取消一封排队的信要等整轮发送结束**（发送锁覆盖整轮），等到时这一轮已经把它发出去了。
- **GUI 运行时 blob GC 从不运行**：只有 `catmail gc` 会调用，而 GUI 占着数据目录。
- **IDLE**：服务器自己立即用 OK 结束 IDLE 时，后台任务一个往返接一个往返地再 IDLE；每次 IDLE 都收到
  无法解析的行（算作变化）时，不停地同步 INBOX。离下次完整同步不到一个 IDLE 时长时，任何安静结束都被
  当成"到期"，同样会循环。`remove_account` 失败（写库失败）后这个账户的后台同步就停了。连接没开 TCP
  keepalive，NAT 静默丢掉连接要等到 IDLE 到期（最长 10 分钟）才发现。
- **操作队列**（没有 MOVE 的服务器）：COPY 后删除原邮件时 EXPUNGE 的答复丢失，用户随后把邮件删掉或
  移回，回放时原邮件已不在，op 变成冲突（还被计为"已完成"），副本留在目标里，下次同步那封被删的邮件又
  出现了。找副本时拿服务器报告的 RFC822.SIZE 去比下载到的字节数，二者不一致的服务器上找不到真正的副本，
  于是再 COPY 一份。目标文件夹换了 UIDVALIDITY 时，移进去还没有 UID 的行被一起删掉（之后会自己恢复，但
  正文要重新下载、期间看不到）。
- 小问题：发信的一轮中途出错时不发 `OutboxChanged`；下载附件或原文后不发事件；`drafts.remote_folder_id`
  的同账户约束没有注明。

## 决策

- **NUL 两层处理**：保存草稿时（新建、修改、回复）从主题、正文、显示名里去掉 NUL，它在文本里没有意义；
  另外 proto 在组装命令失败时返回新的非致命 `Error::InvalidInput`（什么也没发出，连接可用），草稿上传
  把它当作被拒绝记进 `upload_error`，即使将来别的路径带进 NUL 也不会卡住同步。保存到 Sent 遇到它按普通
  失败重试（草稿已清理，实际到不了）。
- **账户状态放在引擎内存里**（`AccountStatus { sync, send }`，`ServerProblem` 记最近的错误、是否登录被
  拒绝、开始时间、连续失败次数），`MailClient::account_status` 查询，出现、清除或错误变了时发
  `AccountStatusChanged`，不是每次失败都发。不持久化：重启后重新尝试就知道。同步方面记录后台同步的每一步
  和用户要求的 `sync_account` / `replay_pending`（真的连了服务器时）；发信方面记录每轮的连接结果。考虑过
  用事件带上错误内容，但现有事件都是"变了、去查"，保持一致。
- **发信先连接、再开始第一封**：连接和登录的失败（含认证失败、TLS、读不到密码）不计入任何一封的尝试次数，
  整轮推迟到同一时刻，间隔按账户连续失败次数退避（复用账户状态里的计数）。放弃了"连不上算暂时、认证失败
  算永久"的旧分类：密码错误是账户的事，改好密码后信应该照常发出。
- **登录被拒绝后的后台同步**每隔 `full_sync`（15 分钟）才再试：这时 `Engine::wake` 什么也不做，所有入队
  的路径一处生效。用户要求的同步或回放成功、清除"登录被拒绝"时唤醒后台任务；别的问题清除时不唤醒（后台
  任务自己恢复时会存下一次唤醒、白跑一轮，而网络问题的退避本来就会被本地改动打断）。
- **取消排队或失败的信不等发送锁**：`Outbox::withdraw` 用带状态条件的 DELETE；尝试只从 `queued` 开始，
  而 `begin_send` 本身是带条件的 UPDATE，二者不会交错。`unknown` / `accepted` 仍然等锁。
- **后台 GC** 是 builder 的选项 `collect_garbage_every(Option<Duration>)`，打开后最多 5 分钟第一次（不与
  启动同步抢，也赶在短暂打开的窗口关闭之前），之后按间隔；GUI 和 `catmail watch` 每 6 小时。没有绑在
  `sync_in_background` 上，测试可以用毫秒级间隔。
- **IDLE**：连续三次刚开始就被服务器安静地结束（不到一半时长、也不到一分钟）就改为轮询；连续超过十次
  IDLE 一开始就报告变化时，每次之间从 `retry` 起翻倍等待、最长 `poll`（真正繁忙的收件箱退化为短间隔轮询）。
  "到期"改为看实际是否过了完整同步的时刻。`remove_account` 失败（不是"账户不存在"）时重启后台任务。
  TCP keepalive 60 秒后开始探测、间隔 15 秒（socket2，tokio 不暴露；重试次数用系统默认，`with_retries`
  要 socket2 的 `all` feature）。
- **丢了 EXPUNGE 答复后找副本**：COPY 的答复带 COPYUID 时，删除原邮件之前用 `write_durable` 记下副本的
  UID（`pending_ops.copy_uid`），原邮件不在时副本就是它，找到就和逐字节找到的一样处理（op 跟过去）；op 没变
  时行直接得到新 UID。没有记下（没有 UIDPLUS）或目标被重建而 op 已被改过时标为 `failed` 并提示用户去目标
  文件夹看一眼，不再悄悄变成冲突。第一版按原邮件的 Message-ID 和大小找，codex review 指出：一封 Message-ID
  和大小都相同的无关邮件恰好在检查点和 COPY 之间到达目标时，会被当成副本，用户的删除或移回就作用到它身上，
  真正的副本反而留下（故障测试复现：删掉了无关的那封）。Message-ID 不能证明身份，所以改为只认服务器给的
  COPYUID；考虑过记原文的摘要，但要在每次 COPY 前下载原邮件。
- 新代际只删有 UID 的行：移进来的行属于源文件夹的位置，照常回放。
- 没做（留到 M5 或以后）：GUI 显示账户状态（M5 界面）、启动时的两次完整同步（有了账户状态后 M5 去掉
  手动那次）、草稿自动保存去抖、Drafts 文件夹改名时丢掉未结算的上传、别处删掉草稿副本本地不知道（单向
  同步的限制）、IDLE 以外顺带报告的 flags 变化要等下次同步、超过 100 MiB 的 IMAP 响应。

- **/simplify 清理**（四个方向的审查）：
  - `Engine::report` 直接接收操作的 `&Result<T>`，各处不再写 `.as_ref().map(|_| ())`；"有没有变"直接与
    原有的状态比较，不用占位值。登录被拒绝时不唤醒放进 `Engine::wake`，去掉 `watch.rs` 里等待的内层循环。
  - 发信一轮的连接状态改为一个枚举（`Smtp::None / Up / Down`），不再是两个互斥的 `Option` 加一个不可能
    的默认值；`send_one` 恢复为一个函数；`is_permanent` 去掉连接之后不再可能出现的分支。
  - `Outbox::withdraw` 检查状态后委托给 `cancel`（也修正了插入时错位的 `cancel` 文档注释）。
  - `find_copies`：已认领的 UID 集合只建一次（按 Message-ID 找的部分后来随 codex review 去掉了）。
  - GC 间隔只在 core 定义一处（`DEFAULT_GC_INTERVAL`），GUI 与 CLI 共用。
  - 测试：`tests/common` 加 `drain`，七处读取积压事件的写法合一；`bodies.rs` 的两个目录遍历合成 `files`。
  - 没采纳：账户状态改在 `connection::connect` 里统一记录（同步要记录整步的结果，而按需下载有意不记录）；
    三个 IDLE 计数合成一个（会把"一直被提前结束就改轮询"改成限速的 IDLE，行为变了）；`watch.rs` 的 IDLE
    结果处理拆成函数；连不上时逐封推迟合成一条
    UPDATE（原有行为，结果要逐封返回）；下载后的事件改为只针对一封邮件（GUI 会多刷新一次列表，代价很小，
    M5 再看）；账户状态不记录本地错误（Store / Io）——状态说的是"同步失败了"，原因照实显示。

## 方案

- `catmail-types`：`AccountStatus`、`ServerProblem`，事件 `AccountStatusChanged`。
- `catmail-proto`：`Error::InvalidInput`（非致命），`append` 组装失败时返回它；`net::configure` 开
  TCP keepalive。根 `Cargo.toml` 加 `socket2`。
- `catmail-store`：`pending_ops.copy_uid`（`PendingOps::copied`），`ReplayOp::copy_uid`；
  `Folders::end_generation` 只删有 UID 的行；`Outbox::withdraw`；`drafts.remote_folder_id` 注释。
- `catmail-core`：`engine.rs`（`Engine::report` / `status`、`Activity`）；`watch.rs`（`Idled`、BUSY /
  CUT_SHORT、认证失败的等待、上报状态）；`send.rs`（先连接、账户退避、`withdraw`、出错也发事件）；
  `ops.rs`（`copy_move` 记下 COPYUID、`find_copies` 按它找、大小比大小、`lost`）；`drafts.rs`（`without_nul`）；
  `draft_sync.rs`；`body.rs`（下载后发事件）；新模块 `gc.rs`；`client.rs`（`account_status`、
  `collect_garbage_every`、删除失败重启后台同步）；`sync.rs`。
- `catmail-cli`：`watch` 打印账户状态、开启后台 GC。`catmail-gui`：开启后台 GC，忽略新事件（M5 显示）。
- `catmail-testkit`：IMAP 故障 `drop_after_expunge`、`deliver_before_copy`、`inexact_sizes`、`end_idle_early`、
  `garble_idle`。
- 文档：`docs/architecture.md`、`sync.md`、`protocol.md`、`sending.md`、`op-queue.md`、`storage.md`、
  `testing.md`，README。
- 验证：每个修复都有一个不修就失败的测试（先在旧代码上确认失败）：core `tests/drafts.rs`
  `a_reply_quoting_a_nul_doesnt_stop_the_sync`；`tests/send.rs` `wrong_password_holds_the_queue_and_is_reported`、
  `a_queued_message_is_cancelled_while_another_is_sent`、`a_pass_that_fails_still_announces_what_it_did`
  （改了 `unreachable_server_postpones_the_rest_of_the_queue` / `postponed_entries_back_off_with_the_one_attempted`
  的期望）；`tests/watch.rs` `a_rejected_login_is_reported_and_not_retried_for_each_change`、
  `idle_the_server_cuts_short_turns_into_polling`、`idle_ending_at_once_every_time_slows_down`（旧代码 3 秒
  71 次 IDLE，新代码 16 次）、`a_failed_removal_keeps_the_account_in_sync`（用触发器让删除失败，测试经
  `tests/common` 的 `sql` 直接改数据库，core 加 rusqlite dev-dependency）；`tests/ops.rs`
  `a_move_whose_expunge_answer_was_lost_finds_the_copy`、`a_lost_expunge_answer_without_copyuid_is_reported`、
  `a_namesake_arriving_with_the_copy_is_left_alone`、
  `a_copy_whose_answer_was_lost_is_not_made_again`（加了 `inexact_sizes`）；store
  `a_new_generation_keeps_messages_on_their_way_in`；`tests/bodies.rs` 下载后的事件、
  `gc_runs_in_the_background_when_asked_to`；proto `a_message_imap_cant_carry_is_not_sent`、
  `connections_are_kept_alive`；CLI `describes_changes` 加账户状态。另补了审查指出缺少的测试：合并的
  `UID MOVE` 里有一封已被别人删除（`a_message_expunged_elsewhere_drops_out_of_a_move`）、上传答复丢失后又
  修改草稿（`a_draft_edited_before_its_lost_upload_is_settled_keeps_the_latest`）。Dovecot / submission
  真实服务器测试通过（`wrong_password_fails_the_send` 改为 `wrong_password_holds_the_message_and_is_reported`）。
  fmt、clippy、全部测试通过。
