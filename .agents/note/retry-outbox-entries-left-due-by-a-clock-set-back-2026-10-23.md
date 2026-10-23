# 系统时间往回调后发件箱仍按时重试

- 日期：2026-10-23
- 类型：修复

## 背景

Issue #84。发件箱的重试时间 `outbox.next_attempt_at` 存的是墙上时钟秒数，后台发件（`send::worker`）
找 `next_attempt_at <= now` 的条目。系统时间往回调之后（NTP 大幅校正、手动改时间），之前被推迟的条目
到期时间留在"未来"：往回调一天，本该最多 30 分钟后重试的邮件一天都发不出去，界面上也看不出来。
复现：SMTP 4xx 让一封被推迟，把它的 `next_attempt_at` 加一天，后台发件不再发它。

## 决策

- 不能像 #55 那样改用内存里的 `Instant`：重试时间要跨重启保存。
- 任何退避都不超过 `MAX_BACKOFF`（30 分钟），所以晚于 `now + MAX_BACKOFF` 的时间只能是时钟往回调留下的。
  时钟只往回调了不到 30 分钟时最多多等 30 分钟，可以接受。
- 起初在查询里判断（`pending` / `accounts_due` 把这样的时间也算到期，用一个 `Due { now, max_wait }`
  类型传参）。/simplify 指出 `next_attempt_at()`（worker 据此决定睡多久）和 CLI 显示仍是旧时间，同一个
  规则要写进三处；改为后台发件每一轮先用一条 UPDATE 把它们改成现在（`Outbox::bring_forward`），查询保持
  `<= now` 不变。被覆盖的时间本来就没有意义。
- 只在后台发件里做：手动 flush 本来就不看到期时间；CLI 不开后台发件，`catmail outbox` 显示的"下次尝试"
  在下一次 flush 之前仍是旧时间，只是显示，不影响发送。
- "任何等待都不超过 `MAX_BACKOFF`"不另加断言：所有写入都是 `now` 或 `now + backoff(..)`，`backoff`
  本身封顶，已有单元测试。
- blob 回收（mtime 在未来的文件不回收）同类但只是暂时多占磁盘，保持现状。

## 方案

- `crates/catmail-store/src/outbox.rs`：新增 `bring_forward(now, max_wait)`。
- `crates/catmail-core/src/send.rs`：`work_due` 在同一个 write 里先 `bring_forward(now, MAX_BACKOFF)`
  再 `accounts_due(now)`，改过的条数记日志。`now` 在 write 闭包里、拿到写连接之后才取（review 指出）：
  在等写连接之前取的话，其间提交的手动 flush 用更晚的时间设了满额退避，会被当成时钟往回调、立即重试。
  写连接只有一条，之前提交的等待都是用不晚于它的时间算的，只有时钟往回调才会超出 `now + MAX_BACKOFF`。
- 文档：`docs/architecture/sending.md` "重试"一节。
- 测试：store 的 `attempts_set_before_the_clock_went_back_are_brought_forward`；`tests/send.rs` 的
  `a_clock_set_back_doesnt_hold_up_a_retry`，后台发件、SMTP 451 让第一封被推迟，把它的 `next_attempt_at`
  加一天，再入队一封唤醒 worker，第一封必须发出。改动前该测试失败。
