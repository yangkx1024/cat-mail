# 引擎自己发现睡眠

- 日期：2026-10-28
- 类型：功能

## 背景

#147。睡眠后要立即追上全靠前端调用 `MailClient::resume`，只有 GUI 调用（`on_system_wake`）；
`catmail watch` 也跑后台同步，睡醒后要等 IDLE 超时、完整同步的定时器才追上。

## 决策

- 引擎定时比较墙上时钟和 `Instant` 各走了多少：Linux、macOS 上前者计入睡眠、后者不计，墙上时钟多走
  60 秒以上算睡过，调用同一个 `Engine::resume`。不用新依赖，所有前端都有效。
- 间隔 30 秒、阈值 60 秒：间隔就是醒来后的最长延迟；阈值远大于漂移和通常的校时，误报（校时、手动调快
  时钟）只多换一次连接、多一次完整同步，墙上时钟往回调不算。更短的睡眠漏掉无妨。
- 醒来时刻记为发现的那一刻，这之间开始的事重做一次，偏保守。GUI 的 `on_system_wake` 保留，醒来即响应；
  上次检查之后已有 `resume`（`since_woke(上次读数)` 为假）时任务不再调用，否则醒来时刻后移，GUI 醒来后
  新开的连接和做完的完整同步都要再来一遍（/simplify 评审指出）。
- issue 原先的第 2 条（计入睡眠的时钟）在 #148 之后不需要，见 issue 评论；没有做。
- 引擎存在期间总起这个任务，不按是否开了后台同步、发信区分：`resume` 也管连接池里的连接，按功能列举
  条件，以后新用到 `since_woke` 的地方都得记得改它（/simplify 评审指出）。
- 检测函数和任务都可注入时钟：任务接收 `now: impl FnMut() -> Clocks`，测试把墙上时钟往前拨。

## 方案

- `crates/catmail-core/src/sleep.rs`：`Clocks`（两个时钟同一时刻的读数）、`slept_until`（判定）、
  `run`（每 `CHECK_EVERY` 比较一次，随引擎结束）。`client.rs` 的 `MailClient::start` 总起它。
- 测试：`sleep::tests::only_a_wall_clock_far_ahead_is_a_sleep`（正常走动、检查被耽搁、小幅校时、
  睡了一小时、往回调）；`client::tests::the_engine_resumes_once_the_wall_clock_jumps_ahead`（两钟同走不
  `resume`，墙上时钟前跳后 `resume` 一次，前端先 `resume` 过则不再调用；去掉 `resume` 调用或
  `since_woke` 检查都会失败）；后台任务随客户端结束的测试多算
  这一个任务。睡醒后在新连接上完整同步、到期发信由 #148 的 `resume` 测试覆盖。没有在真机上测睡眠。
- 文档：`docs/architecture/background.md`"引擎自己发现睡眠"。
