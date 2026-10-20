# M4（二）：IDLE 推送与后台同步

- 日期：2026-10-19
- 类型：功能

## 背景

到 M4 第一部分为止，邮件只在用户运行 `catmail sync` 或点"同步"时更新，本地的标记 / 移动 / 删除
也要等下一次同步或 `ops flush` 才到服务器（CLI 是当场回放）。GUI 这样的长期运行前端需要新邮件
自己出现、改动立刻生效。架构文档原先设想的是"每账户一个 actor（1 条 IDLE 连接 + 1~2 条工作连接，
通过 mpsc 接收命令）"。草稿同步到服务器是 M4 的最后一个 PR。

## 决策

- **每账户一个后台任务，不做 mpsc actor。** 任务只有一条连接：定时完整同步，其间在 INBOX 上 IDLE，
  本地改动入队时被唤醒去回放。它和手动同步、`replay_pending` 共用已有的每账户同步锁，只在干活时
  持有、等待时不持有。放弃把所有操作都经 actor 转发：门面方法已经按锁串行，按需下载有连接池，转发
  不增加保证，反而让打开邮件排在同步后面；也不会出现"用户点同步要等 IDLE 结束"。
- **只盯 INBOX。** 一条连接只能 IDLE 一个文件夹，NOTIFY（RFC 5465）支持的服务器太少；其他文件夹每
  15 分钟完整同步一次（`SyncSchedule::full_sync`，任何完整同步都算，包括手动的）。不支持或拒绝
  IDLE 的服务器每 2 分钟检查 INBOX。IDLE 每 10 分钟重新开始，及时发现无声断开的连接。
- **IDLE 之前漏掉的变化**：服务器只报告一次变化，可能夹在同步 INBOX 时的 FETCH / SEARCH 答复里。
  连接层统计每个答复里的 EXISTS 变化、EXPUNGE、VANISHED，`Session::idle` 发现打开文件夹以来已有
  变化就立即返回，后台任务再同步一遍。考虑过 IDLE 前再 EXAMINE 一次比对游标：没有 CONDSTORE 时
  每轮要多拉一次全部 flags，也仍有窗口。
- **开关在 builder**：`sync_in_background(Option<SyncSchedule>)`，与 `send_in_background` 一致，默认
  关闭。GUI 打开；CLI 只在新命令 `catmail watch` 里打开（其他命令是一次性的，仍当场回放）。间隔放进
  公开的 `SyncSchedule`，测试用毫秒级间隔。
- **错误只记日志**，不加事件：GUI 启动和添加账户时仍手动同步一次，把错误显示成通知。后台失败从
  30 秒起翻倍重试（最长 15 分钟），有本地改动入队时立即再试；IDLE 中断线、复用的连接已断开时立即
  重连。显示连接状态留到 M5 的界面。
- **生命周期**：引擎打开时为已有账户、`add_account` 时为新账户启动任务；任务只持有引擎的弱引用，
  账户的取消信号在引擎 drop 时也触发，最后一个客户端 drop 后立即结束并关闭连接，不让引擎和数据目录锁
  多活一个 IDLE 周期。账户删除时 IDLE 中也立即结束，删除不必等。
- **IDLE 安静地到期就直接再 IDLE**，不同步 INBOX：往返本身就能发现断开的连接，而没有 CONDSTORE 的
  服务器（如 Exchange）同步一次要拉全部 flags。
- **假服务器的 IDLE 模拟真实服务器**：每条连接记住所选文件夹的样子，任何命令的答复里顺带报告新邮件，
  IDLE 时报告 EXPUNGE / EXISTS / flags，UIDVALIDITY 改变时 BYE。顺带报告新邮件对所有测试都开着，
  已有测试不受影响。

- **/simplify 清理**（四个方向的审查）：
  - proto：顺带报告的变化从计数器加基线改为连接自己的一个标志（`mailbox_opened` 重置），EXISTS 只存
    一处；三处开始命令的代码合成 `begin`；IDLE 的读取复用 `read_untimed`。
  - core：去掉 `Engine::gone`，改为账户取消信号在引擎 drop 时也触发；去掉监督任务（原先监听事件流找
    `AccountAdded`，要处理滞后和去重），改在打开和 `add_account` 时启动；"复用的连接已断开就在新连接上
    重来一次"与连接池共用 `connection::on_session`；"拿同步锁、账户已删除就失败、删除时取消"合成
    `Permit::run`（同步、回放、发信、后台同步四处）；`announce` 总是唤醒，不再分 `announce_queued`，
    以后入队的路径不会忘记；`sync_inbox` 按角色找 INBOX、只读一行同步状态（`Folders::sync_state`）；
    重试间隔与 outbox 共用 `engine::backoff`；CLI `watch` 合并积压的事件，每个文件夹一行。
  - 测试：`eventually`、`id_of` 移进 `tests/common`，`ServerState::received` 按前缀匹配，`wait_until`
    10 秒超时，CLI 测试的建账户移进 `test_support`。
  - 没采纳：完整同步后 INBOX 紧接着再同步一次（为了让它最后选中再 IDLE，15 分钟一次，代价小）；GUI
    启动时与后台任务各做一次完整同步（要等后台同步能报告错误，M5）；后台任务一步之内持有引擎的强引用
    （只推迟引擎释放到这一步结束）；假服务器的 `new_messages` 与 `View::catch_up` 合并（测试代码）。

- **codex review 修复**：服务器或代理接受登录和 EXAMINE、却每次都关掉 IDLE 的连接时，后台任务每次
  都立即重连（同步成功就清零失败计数，IDLE 断线又绕过了重试间隔），不停地登录、同步。先用假服务器的
  新故障 `drop_on_idle` 复现（0.3 秒内重连 423 次），再修：连续的 IDLE 在开始后不到一分钟就断线时
  计数，第一次仍立即重连（偶尔一次断线照常恢复），从第二次起按 `retry` 翻倍退避；IDLE 正常结束或
  撑过一分钟才断开就重新计数。

## 方案

- `catmail-proto`：`Connection::idle`（imap-next 的 IDLE 事件，等待时 `select!` 读取、`stop` 和
  `max`，然后 DONE）；`MailboxUpdates` 统计顺带报告的变化，SEARCH 用它判断文件夹是否为空（取代
  修改命令里单独维护的 EXISTS）；`Session::can_idle` / `idle`。
- `catmail-core`：新模块 `watch.rs`（`SyncSchedule`、每账户任务）；`sync::run` 改为在给定
  连接上同步并记录完成时间，新增 `sync_inbox`；`Engine` 加 `wake` / `synced`；`ops::announce`
  唤醒后台任务（入队的改动，含回复后的 `\Answered`）；builder 的 `sync_in_background`。
- `catmail-cli`：`catmail watch`（打印每次变化后的文件夹计数，`--json` 输出事件）。
- `catmail-gui`：打开后台同步。
- `catmail-testkit`：IDLE、顺带报告新邮件、`Faults::idle`、`open_connections`、`FakeImap::wait_until`。
- `testenv/dovecot/run.sh`：新增 `push` 用户。
- 验证：proto 4 个（IDLE 报告新邮件 / flags / 删除并可停止或超时、之前顺带报告的变化立即返回、断线
  与 BYE、不支持 IDLE）；core `tests/watch.rs` 10 个（IDLE 一再断开时退避、安静的 IDLE 到期直接再 IDLE、推送新邮件 / flags / 删除且不做完整同步、改动
  立即到服务器、断线重连、无 IDLE 轮询、定时完整同步、登录失败后重试、随账户删除结束、随客户端结束
  且打开时就监视已有账户），确认去掉唤醒后"改动立即到服务器"失败；CLI 1 个；Dovecot 测试往 `push`
  的 maildir 投递，不到一秒出现在本地。手动用 `catmail watch` 对 Dovecot 验证。fmt、clippy、全部测试
  通过，Dovecot / submission 真实服务器测试本地通过。
