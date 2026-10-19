# 修复 M3 之后的遗留问题：发信、存储、同步与 CLI

- 日期：2026-10-19
- 类型：修复

## 背景

M3 合并后、开始 M4 之前，对照 AGENTS.md 里"容易违反的规则"审查了一遍代码，找到一批会在
实际使用中出问题的地方，其中两条违反"不在用户不知情时重发"：

- SQLite 用 WAL + `synchronous=NORMAL`，断电或系统崩溃会丢掉最近几次提交。写下 `unknown`、
  发出结束标记、服务器收下后断电，重启时条目退回 `sending`，被当成没发出去而重发。
- 结束标记之后的答复只要不是 250 就当作拒绝，于是其他 2xx、3xx 之类的答复会让邮件重新入队、
  再发一次。

其余：EHLO 回 4xx（服务器忙、限流）被当成永久失败；读不到密码（密钥环被锁）也是永久失败，
后台发送会每轮烧掉一封；连不上服务器时其余邮件统一推迟 30 秒，下一轮由它们轮流去连，退避
失效；第一次保存到 Sent 就先 EXAMINE / SEARCH（`begin_save` 已经计过一次，`attempt_save`
又加一）；尝试没能记下结果（写库失败、flush 被取消）的条目停在 `sending`，只有重启能恢复；
被推迟的条目不在 flush 结果里，`catmail send` 在服务器连不上时报 "unknown error"；账户自己的
地址没按发件地址检查，要入队后才在 `MAIL FROM` 失败；数据目录与 `catmail.db` 其他用户可读；
LIST 响应缺了 INBOX 那一行（被截断或损坏）时照样对账，删掉所有没列出的文件夹和邮件；`compose` / `reply`
在读正文文件、添加附件失败时留下没人知道的草稿；`outbox flush` 总是返回 0，一个账户出错就
丢掉其他账户的结果；部分命令在 `--json` 下不输出结果；`nom` 没走 workspace 依赖；README 和
GUI 说"和 CLI 一样只读"，CLI 从 M3 起会发信。

## 决策

- **只对关键提交 `synchronous=FULL`**（`Store::write_durable`：写 `unknown`、保存到 Sent
  的尝试计数）。起初全局打开 FULL，/simplify 审查指出同步时每批邮件的提交都会多一次 fsync，
  单连接上 GUI 的读要排在后面；WAL 下一次 FULL 提交会把之前的提交一起落盘，所以只在发结束
  标记、APPEND 之前那一次提交 FULL 就够了。blob 文件本身已 fsync；目录项没 fsync，断电最坏
  是 blob 丢失、条目报"已存邮件丢失"，不会重发，暂不处理。
- **结束标记后的答复按类别判断**：2xx 接受，4xx / 5xx 拒绝，其余返回 `Protocol` 错误（致命、
  非 `Reply`），引擎按 `unknown` 处理。宁可让用户确认，也不自动重发。
- **密钥环错误算暂时失败**。也考虑过"连接层面的失败（连不上、登录、密码）不算到任何一封
  邮件头上，整轮推迟"，但这会改变错误密码的现有行为（现在是队首 `failed`、提示用户），
  改动面大，没做；架构文档里注明这是权宜之计，M5 开启后台发送前再改。副作用：密钥环里
  密码不见了也会一直按退避重试（`last_error` 提示重新添加账户），不再判失败。
- **推迟到失败那封的下次尝试时刻**，并把被推迟的条目放进 flush 结果。
- **陈旧的 `sending` 在拿到发送锁时就地退回队列**（flush 和用户决定都经过同一个
  `idle_outbox`）：持有发送锁时不可能有尝试在进行，`sending` 只能是没收尾的。启动时的全局
  恢复保留，否则后台任务看不到只有 `sending` 的账户。
- **数据目录新建时 0700**（只设目录本身，上级照常创建，避免把 `~/.local` 也设成 0700），
  已存在的目录不改：用户自己指定的目录不擅自改权限；项目未发布，没有旧数据目录要迁移。
  Windows 依赖用户目录的 ACL。
- **LIST 缺 INBOX 时先 EXAMINE INBOX 确认**：能打开说明响应不完整，返回 `Incomplete`、
  整次同步报错，不做"只新增、不删除"的部分对账；打不开说明这个用户没有 INBOX，结果照常
  使用。最初无条件报错，codex review 指出 RFC 3501 §6.3.8 只要求"用户有 INBOX 时"列出它
  （判据就是 SELECT INBOX 是否失败），无条件报错会让这类账户永远同步失败、也找不到 Sent。
  EXAMINE 只读，且正是 RFC 给的判据；没加 STATUS（假服务器和 proto 都还没有）。检查放在
  proto 的 `Session::list`，所有调用方都拿到完整的结果或错误；store 的对账保持纯粹。
- **CLI**：`compose` / `reply` 失败时删掉刚建的草稿（删除失败则在错误里说明草稿 id）；
  `reply` 先读正文再建草稿。`outbox flush` 与 `send` 一致，有邮件没发出去就非零退出，并收集
  各账户的错误而不是遇错即停。`--json` 下有返回值的命令都输出它（`account remove`、
  `draft attach`、`outbox cancel`，`read --save` 加 `saved` 字段）；只删除、没有返回值的
  （`draft rm` / `detach`）不输出，以退出状态为准。
- 留给 M4：`pending_ops` 随文件夹级联删除、`drafts.remote_folder_id` 改复合外键。留给以后：
  超过 100 MiB 的 IMAP 响应、下载附件后不发事件、发送锁的粒度（M5 开启后台发送前）。

## 方案

- `catmail-store`：`Store::write_durable`；`Outbox::recover_interrupted` 加账户参数。
- `catmail-proto`：`smtp/session.rs` 的 EHLO 与 `end_message` 按答复类别区分；
  `imap/session.rs` 的 `list` 缺 INBOX 时返回 `Incomplete`。
- `catmail-core`：`send.rs`（发件地址检查、`idle_outbox`、推迟到失败那封的下次尝试时刻并
  放进结果、`Secrets` 不再永久、首次保存不搜索、`unknown` 与保存计数用 `write_durable`）；
  `engine.rs`（`write_durable`）；`client.rs`（`create_data_dir`）。
- `catmail-cli`：`compose.rs`、`main.rs`。根 `Cargo.toml` 加 `nom`。
- `catmail-testkit`：假 SMTP 加 `ehlo_busy`；`end_of_data_replies` 为 2xx 时照常投递；
  按次数触发的故障共用 `take`。
- 文档：`docs/architecture.md`（草稿与发信、同步、数据目录）、README、GUI `main.rs`。
- 验证：每个修复都有一个不修就失败的测试（先在旧代码上看到失败）：proto
  `busy_ehlo_is_temporary`、`end_of_message_verdicts`、`unparsable_lines_make_authoritative_answers_fail`
  （LIST 缺 INBOX 行，及没有 INBOX 的用户）；core `tests/send.rs` 的 `odd_reply_to_the_end_is_not_taken_as_a_refusal`、
  `postponed_entries_back_off_with_the_one_attempted`，`sends_and_saves_to_sent` 断言首次
  保存不 EXAMINE / SEARCH，`unreachable_server_postpones_the_rest_of_the_queue` 断言推迟的
  条目在结果里；`client.rs` 单测 `sends_left_unfinished_are_requeued_without_a_restart`、
  `locked_keyring_postpones_sending`、`account_address_is_checked_before_queueing`、
  `data_dir_is_private`（Unix）；store `durable_writes_are_synced`；`tests/sync.rs`
  `listing_without_inbox_deletes_nothing`、`account_without_inbox_syncs`；CLI `failed_command_leaves_no_draft`。
  fmt、clippy、全部测试通过，Dovecot 真实服务器测试本地通过。
- /simplify（四个方向）之后的清理：上面两处层次调整（按需 FULL、INBOX 检查下沉到
  proto）；推迟时刻直接取失败那封入库的 `next_attempt_at`，去掉另算一遍退避的 `Down`；
  首次保存用布尔标记代替计数；测试辅助函数合并；CLI `outbox flush` 的分支与成功判断
  （`went_out`）。没采纳：用户决定与陈旧 `sending` 的恢复合并成一次事务、推迟的条目一次
  写入、CLI 先检查附件路径（收益太小或与 core 的检查重复）。
