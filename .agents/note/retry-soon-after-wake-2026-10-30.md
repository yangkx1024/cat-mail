# 睡眠醒来后先快速重试，并记下前端的唤醒

- 日期：2026-10-30
- 类型：功能

## 背景

真机休眠一夜后的日志：唤醒 4 秒后两个账户都因 DNS 解析失败（网络还没连上）连不上，按平常的重试间隔
等了 30 秒才同步，从醒来到邮件更新约 36 秒。发件箱醒来后立即发到期的邮件，连不上 SMTP 时同样从 30 秒起
等，睡前就连不上的还要等几分钟。GUI 经 `on_system_wake` 调用的 `resume` 不记日志，只能从
"引擎自己发现睡眠"那条日志没出现倒推是 GUI 先处理的。

## 决策

- 醒来后连不上服务器的失败先从 2 秒起翻倍，到平常的第一次间隔为止（2、4、8、16 秒），之后照常翻倍。
  只计醒来后的失败：睡前就连不上的不该因此在醒来后等更久。成功一次就结束，之后的失败照旧等。快速重试
  的几次插在平常的间隔前面，不计入后面的退避，否则 2 分钟后已退到 15 分钟。
- 计数放在 `Engine::report`，每个服务器一份，同步和发件箱共用 `Failures::retry_in`。最初只放在后台同步
  的任务里，/simplify 评审指出发件箱有同样的问题，挪进引擎后两边一套规则，不重复。不改
  `ServerProblem::failures`：那是前端显示的状态。
- 醒没醒过用 `Engine::since_woke` 比较上次记录的时刻，不另开读取醒来时刻的接口（/simplify 评审指出）；
  从没记录过的服务器按账户状态建立的时刻算。
- 放弃监听网络变化（NetworkManager / `nw_path_monitor` / `INetworkListManager`）：每个平台各写一份，Linux
  上不一定有 NetworkManager，连上网络也不等于连得上服务器。快速重试三个平台相同、无新依赖。
- 发件箱里某一封连上之后的暂时失败不变，按它自己的尝试次数退避。
- 登录被拒绝算连上了服务器，结束快速重试（Codex 评审指出：SMTP 密码失效时醒来后 30 秒内会被拒五次，
  有锁定账户的风险）。在引擎里判断而不是在 `send.rs` 特判，IMAP、SMTP 一样。快速重试的间隔也受最长间隔
  限制（Codex 评审指出：`full_sync` 比 `retry` 短时会超出）。
- 日志记在 `MailClient::resume`（前端的来源），引擎自己发现睡眠的那条已有；挪进 `Engine::resume` 会记
  两遍或分不清来源。

## 方案

- `crates/catmail-core/src/engine.rs`：`Status` 每个服务器一个 `AfterWake`（上次记录的时刻、醒来后连续的
  失败数），`report` 返回 `Failures { in_a_row, after_wake }`，`Failures::retry_in` 算等待。
  `watch.rs`：`SyncSchedule::retry_after_wake`，失败时用 `retry_in`。`send.rs`：连不上时用 `retry_in`
  （`RETRY_AFTER_WAKE` 2 秒）。`client.rs`：`resume` 记一条 info。
- `catmail-testkit`：`Faults::refuse_connections`，问候之前关掉接下来这么多条新连接，模拟网络没通。
- 测试：`engine::tests::failures_after_a_wake_are_tried_again_soon_then_as_usual`（睡前的失败照常、醒来后
  2/4/8/16/30/60/120、成功后照常、不超过最长间隔）；`tests/watch.rs` 的
  `failing_to_connect_after_a_wake_tries_again_soon_until_through`（`retry` 设为一小时，醒来后三次连不上
  仍很快 IDLE 上；成功后再连不上就不再快速重试）；`tests/send.rs` 的
  `an_unreachable_server_is_tried_again_soon_after_a_wake`（醒来后 SMTP 连不上推迟几秒；发出去之后再连不上
  照常 30 秒）、`a_login_rejected_after_a_wake_is_not_tried_again_soon`（醒来后登录被拒仍等 30 秒）。
  去掉快速重试或成功后不结束，前两个集成测试都会失败；不把被拒绝当作连上，最后一个失败；快速重试不受
  最长间隔限制，单元测试失败。
- 文档：`docs/architecture/background.md`"系统睡眠与唤醒"、`sync.md`"失败"、`sending.md`。
