# 后台预取正文

- 日期：2026-11-01
- 类型：功能

## 背景

正文只在打开时下载，没打开过的邮件只能按主题和地址搜到，正文里的内容搜不到（#195）。直接在同步后把所有正文
拉下来又有问题：Gmail 一天经 IMAP 下载 2500 MB 就暂停账户的 IMAP（通常 1 小时，最长 24 小时），网易也有不公布
数值的 IMAP 每日流量上限、两千多封就可能碰上，锁住期间同步和打开邮件都不能用；整封下载要存原文和解码后的
附件，本地占用约为邮箱大小的 1.5–2 倍；预取若用连接池，打开邮件要排队；一封总是取不下来的邮件会被反复重试、
卡住后面的。

## 决策

- **开关在 builder 上**（`MailClientBuilder::prefetch_bodies(Option<BodyPrefetch>)`，默认关），GUI 和
  `catmail watch` 打开。不挂在 `sync_in_background` 上：许多测试开着后台同步，多出的连接和下载会改变它们看到的
  服务器行为；一次性的 CLI 命令也不该在背后下载。
- **每账户一条自己的连接**，不用连接池（池里只有 2 条，打开邮件不能等预取）；没事做时关掉。不拿同步锁。
- **小邮件整封、大邮件只取正文**：`whole_limit` 默认 256 KiB，大多数邮件整封下载，离线也能看附件、留下原文；更大的
  走按部分下载的路径。单个正文部分的上限仍是 `whole_message_limit`：考虑过两个界限用一个数，但那样超过 256 KiB 的
  HTML 会被当成附件存下，打开时也不再取。
- **每日预算按服务器分，按 UTC 日记在数据库里**（`prefetch_usage`），重启后接着算。Gmail 1 GiB：跨零点两边各
  取满也只有 2 GB，低于 2500 MB 的限额，余下的留给打开邮件和其他客户端。其他服务器 200 MiB：多数不公布限额，网易
  确认有、数值未知，按保险定。看账户的 `gmail_server`（连上过的服务器是 Gmail），不看 Gmail 模式：限额属于服务器，
  隐藏了"所有邮件"的 Gmail 账户也一样。没有另设"只取最近多久"的范围：预算已经限住每天的量，更早的邮件由 #196
  （问服务器）补上。
- **服务器说超限了就停 24 小时**，作为预算之外的兜底（限额未知、其他客户端用掉了额度）。按回复的文字认（Gmail 的
  `bandwidth limits`，网易的 `Fetch volume limit exceed` / "flow over limit"），不论出现在登录、EXAMINE 还是
  FETCH 上。不给邮件记失败；暂停时刻写进 `prefetch_usage.paused_until`，重启后也等。没认出的超限回复由"连续三封
  被拒停一小时"兜住。只停预取，同步和打开邮件照常。
- **日志**：状态变化（启动参数、每批用量与预算、预算用完、超限的服务器原话、暂停到何时、连续被拒、账户级失败、
  全部取完）在 info / warn，默认进日志文件；逐封的大小、下载字节、整封与否、耗时在 debug。出了超限之类的问题，
  看日志就能知道当时用了多少、服务器说了什么。
- **等一次完整同步之后才开始**，首次同步时不与拉邮件头抢带宽。完整同步结束时什么都没变就一个事件也不发，所以
  引擎加了每账户的 `synced_signal`（`Notify`），而不是让预取轮询。
- **失败分两种**：服务器拒绝这封邮件（NO、解析不完整、说有却不给）记进 `prefetch_failures`，1 小时起每次乘 4、
  最长一周后再试；连着三封都这样就整个停一小时（多半是账户级的拒绝，不再给后面的记失败）。断线、超时只按
  `retry` 退避，不记在邮件上。登录被拒不自己重试（每次都算一次失败的登录），等被拒之后有一次完整同步成功。
- **文件夹被重建**（UIDVALIDITY 变了）放进内存里的跳过名单，等下一次同步；否则每封都要多一次 EXAMINE。
  为此 `body.rs` 用 `folder_rebuilt` 把它和"邮件已不在服务器上"分开（两者都是 `OutOfSync`）。
- **一批一个事务、每个文件夹一次事件**，默认 20 封一批；一批中途出错，已取到的照样写下。
- schema 原地改 `0001_init.sql`：0.1.0 只有作者自己在用。

## 方案

- `catmail-core/src/body.rs`：`on_server` 拆出与连接无关的 `in_folder`（EXAMINE、取信、重开一次、判断已删除或
  被漏掉），连接池路径和预取共用；`fetch` 改收 `Limits { whole, text }`；新增 `fetch_body`、`parse`、`Fetched::len`、
  `folder_rebuilt`。
- `catmail-core/src/prefetch.rs`：`BodyPrefetch` 和每账户任务（等同步、取一批、存一批、按结果等待）；与后台同步在
  同样的地方启动（`MailClient::start`、`add_account`、撤销删除）。`engine.rs` 加 `prefetch` 字段和 `synced_signal`。
- `catmail-store`：`prefetch_failures`、`prefetch_usage` 两张表和 `Prefetch` repository（`next` 从新到旧、排除
  已删除 / 垃圾邮件 / 跳过的文件夹 / 未到期的失败，`failed`、`used`、`add_used`、`pause`、`paused_until`）；
  `store_body` 删掉失败记录。
- `catmail-testkit`：新故障 `refuse_body_fetch`（指定 UID 的正文 FETCH 一律回 NO）、`body_fetch_refusal`（所有正文
  FETCH 回指定的 NO 文字）和 `hold_body_fetches`。
- 真实 Dovecot：`testenv/dovecot/run.sh` 加 `prefetch` 用户，`tests/dovecot.rs` 加
  `bodies_are_prefetched_from_dovecot`。
- 文档：`bodies.md` 新增"后台预取"一节（含各家限额的查证结果和来源），`storage.md`、`architecture.md`、
  `testing.md`、README 相应修改。
- 顺带发现网易要求先发 IMAP `ID`，否则 EXAMINE 回 `Unsafe Login`，我们不发：另开 #197。
- 预取让正文更早按当时的清洗规则定格，按规则版本重建旧正文另开 #198。
- **/simplify 整理**（复用、简化、效率、层次四个角度）：
  - 候选查询加部分索引 `messages_prefetch`（10 万封时每批约 17 ms → 0.1 ms，有测试钉住查询计划）；一批内按文件夹
    排序，少切换 EXAMINE。
  - 超限识别移到 proto（`imap::Error::is_over_limit`、`says_over_limit`），core 的 `Error::is_over_limit` 转发；
    同步和账户状态也该认它，超出本次范围，另开 #199。
  - 去掉与 `Paused(IDLE)` 等价的 `Batch::Refused`，摊平任务循环里的嵌套 match；用量与暂停合成一次查询
    （`Prefetch::usage`），`pause` 改成 UPDATE；两处启动每账户后台任务合成 `spawn_background`；测试改用
    `id_of` 和公共的 `Env::query`，store 测试的 id 查询移进 `test_util`。
  - 没采纳：`OutOfSync` 带原因枚举（改公开错误类型）、`synced` 换成 watch channel、预算用完后只睡不听事件
    （系统睡眠时定时器会晚到）、`next` 直接返回位置（省下的只是微秒）。

验证：`cargo fmt`、`clippy -D warnings`、全部测试通过。新增 store 测试 4 个（候选顺序与排除、失败退避与正文写入后
清除、按日累计用量及账户已删除时不写、暂停跨日保留）、core 单元测试 1 个（认出超限回复）、core 集成测试 11 个
（同步完所有文件夹前不取、小邮件整封大邮件只取正文、不碰已删除文件夹且不标已读、新邮件到达即取、一批一个事件、
按预算从新到旧、Gmail 用自己的预算、被拒的邮件不卡住后面、连续被拒暂停、超限后停下且重启后仍停、断线重试、文件夹
重建后不空转并在同步后继续、登录被拒后不再登录直到同步成功）。对排除已删除文件夹、跳过重建的文件夹、连续被拒暂停、
记录失败、预算、同步门槛、整封界限、登录被拒后的等待、超限识别、暂停持久化、Gmail 预算选择做了变异测试，确认对应
测试会失败。
本地对 Dovecot 2.4.5 跑了 `dovecot` 和 `submission` 测试（新增的预取测试对同一组服务器重复运行也通过）；
没有对真实 Gmail 账户试过。
