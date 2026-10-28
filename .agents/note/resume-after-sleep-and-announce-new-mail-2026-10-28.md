# 睡眠唤醒后立即重连，引擎判定新邮件

- 日期：2026-10-28
- 类型：功能

## 背景

#106 第三项。GUI 关窗后常驻（#145），系统睡眠就成了常见情况：Linux、macOS 上 `Instant` 和 tokio
定时器不计睡眠时间，醒来后完整同步、发件箱重试都要晚到；后台任务接着用 IDLE 的那条连接，它多半已经
断了，结束 IDLE 的 DONE 等不到答复，要等超时才发现。系统通知（#82）还需要知道哪些邮件是"新到的"。

## 决策

- `resume` 只记下醒来的时刻，连接和同步时间都记着自己从什么时候开始，用到时比较，早于醒来的不算数。
  最初的做法是醒来时清空连接池、忘掉同步时间、发信号让任务丢掉连接，但信号不带时间、清空只管当下：
  一步跨过醒来这一刻时，睡前开始的完整同步做完会把自己记成刚同步过（Codex 评审指出，只会同步 INBOX），
  前台操作会把睡前的连接放回清空了的池子，任务也会丢掉醒来后才新开的连接。三处补丁都不如换成比较。
  考虑过用计数器（"睡眠纪元"），但 `Instant` 本来单调，睡前的时刻一定早于醒来时记下的，不必另造。
- `resume` 信号仍是每账户的，不复用 `wake`：`wake` 结束 IDLE 时发 DONE，正好是会卡住的那一步。信号只叫
  任务看一眼：IDLE 的连接是睡前建立的才丢掉 IDLE 的 future 和连接。一步正在进行时不取消它（中途丢弃
  可能丢掉提交后的事件），信号留到下次等待。
- 登录被拒绝时 `wake` 和 `resume` 都不理会，改在收的一方判断（`watch::unless_rejected` 查
  `Engine::login_rejected`），不在发的一方：一步还在登录时发来的通知会排着，等这一步得知被拒、开始等待时
  才被取走，只在发的时候过滤挡不住（Codex 评审指出，`wake` 原先就有这个问题）。
- `set_synced` / `set_folder_synced` 先取完成时刻、再查同步开始时是否在醒来之后：查过之后才醒来的，
  醒来的时刻晚于记下的，读出时被滤掉；先查后取时刻则会漏掉（Codex 评审指出）。这条规则收在
  `engine::Woke::finished` 里，时钟由调用方传入，单元测试 `work_begun_before_a_wake_never_counts` 在取时刻
  的那一刻、检查与记下之间各插入一次唤醒；改回先查后取，前一种情况失败。
- 新邮件在 `fetch_new` 每批提交后判定、发出，而不是同步结束后汇总：中断的同步也不会漏发，重来时已存的
  行不再插入，也不会重发。判定只看新插入的行（`Upserted.added` 改为行 id），Gmail 上移回来的行不算。
- 已知误报：服务器不给 COPYUID 时本地移进 INBOX 的未读邮件会被当成新邮件，少见，记在文档里。
- 只有 GUI 调用 `resume`，`catmail watch` 不听系统唤醒，记在 #147。

## 方案

- `catmail-types`：`Event::NewMail { account, messages }`；`ServerProblem::login_rejected`，`wake`、`resume`、`record` 共用。
- `catmail-store`：`Upserted.added: Vec<MessageId>`；`Messages::new_mail(ids, from_uid, inbox)`（UID ≥
  原 UIDNEXT、未读、在 INBOX 位置或 Gmail 上"所有邮件"里带 INBOX 标签；INBOX 取 `by_role` 选的那个，
  这批没有新插入的行就不查）。
- `catmail-core`：`sync::fetch_new` 带上原 UIDNEXT（首次同步、UIDVALIDITY 改变时没有）；
  `Engine::resume` / `MailClient::resume`（同步方法，不会失败）记下 `woke`，`Engine::since_woke` 比较；
  `set_synced` / `set_folder_synced` 带上同步开始的时刻；`connection::on_session`（连接池和后台任务
  都经过它）换掉睡前建立的连接；`watch.rs` 在 IDLE 和等待时 select `resume` 信号，IDLE 的连接是睡前
  建立的才丢掉（`slept_through`）。
- `catmail-proto`：`imap::Session::opened`，连接建立的时刻。
- `catmail-gui`：`Resident` 在 `on_system_wake` 里调用 `resume`。CLI `watch` 打印 / 输出 `new_mail`。
- `catmail-testkit`：`ServerState::silence_open_connections`，模拟睡眠中断掉、不再应答的连接。
- 测试：store 的两个 `new_mail` 单元测试；`tests/sync.rs`（只报 INBOX 未读新到的、只报一次、
  UIDVALIDITY 改变不报、resume 后打开文件夹会同步）、`tests/gmail.rs`（带 INBOX 标签的才算、移回的不算）、
  `tests/watch.rs`（resume 后在新连接上完整同步、睡前开始的完整同步做完后仍再做一次、醒来后新开的连接
  不丢、登录被拒的不因唤醒重试、还在登录时排着的改动和唤醒在被拒后不重试）、`tests/bodies.rs`（醒来时正用着的连接之后不再复用）、`tests/send.rs`（resume 后
  到期的重试立即发出）。把行为改回清空 + 信号，"新开的连接不丢""不再复用"两测失败；去掉
  `set_synced` 对开始时刻的检查，"睡前开始的完整同步"一测失败。没有在真机上测睡眠。
- 文档：`docs/architecture/background.md`、`sync.md`。
