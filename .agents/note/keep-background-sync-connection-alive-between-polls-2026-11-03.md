# 后台同步在两次检查之间保持连接

- 日期：2026-11-03
- 类型：修复

## 背景

issue #227：163 账户几乎每 2 分钟重新登录一次（每次 2.5–7 秒），日志里满是
`reused connection was dead`。163 不支持 IDLE，后台同步每 `poll`（2 分钟）检查一次 INBOX，两次之间
连接空闲约 115 秒，而真实服务器探测（只读命令）显示 163 有两个计时器：

- 60–70 秒没有任何命令就断开（RST），NOOP、EXAMINE、UID SEARCH 都重新计时；
- 约 185–213 秒没有"实际"命令就 `BYE Autologout; idle for too long`，NOOP 不算，EXAMINE / UID SEARCH 算。

issue 起初怀疑的连接池 keepalive（4 分钟）也撑不过这两个计时器，但两天的日志里 163 没有一次按需下载正文，
复用失败的代价没落到用户身上。

## 决策

- 只修后台同步：不在 IDLE 的等待中每 `SyncSchedule::keepalive`（45 秒）发 NOOP。每次检查本身是
  EXAMINE INBOX，长计时器随之重新计时，连接可以一直用下去。对所有不支持 IDLE 的服务器通用，不必认出
  Coremail；对别家只是每 45 秒一个往返。
- 不为 Coremail 单独调连接池（45 秒 NOOP、最后一次使用约 3 分钟后关掉）：只在"上次使用后 60 秒到约
  3 分钟内又打开一封没下载的邮件"时省一次登录，现在几乎碰不到；要让池知道服务器是哪家、按账户配参数，
  阈值是一天探测得来的。也不伪造 EXAMINE 绕过 autologout。分析和探测数据在 #227 的评论里。
- 没有缩短 `poll`：更频繁的检查比 NOOP 贵。
- NOOP 不和唤醒信号 `select!`：命令不是 cancel-safe，打断会让答复读到一半、连接没法再用。信号是
  `notify_one`，留着 permit，NOOP 之后的等待立即结束；只有账户删除 / 引擎关闭会打断它，那时连接一起丢掉。

## 方案

- `catmail-core/src/watch.rs`：`SyncSchedule::keepalive`；`run` 末尾的等待改成循环，有连接且下一次
  NOOP 早于等待结束时先睡到那时，再 `keep_alive`：睡前建立的直接关掉（`pool::usable` 改为 `pub(crate)`），
  否则发 NOOP。NOOP 本身（计时、致命错误才关掉、被拒绝照常保留、日志）从池的 keepalive 里抽成
  `pool::ping`，两边共用，日志用 `whose` 字段区分。`engine.rs`、`protocol.md` 里"服务器踢掉闲置连接要久得多"
  补上网易这个例外。
- `catmail-testkit`：`Faults::idle_timeout`，连接一段时间没有命令就不声不响地断开。
- 测试（`tests/watch.rs`）：`keeps_the_connection_open_between_polls`（服务器 1 秒断开空闲连接、2 秒
  检查一次，新邮件仍在同一条连接上同步到；去掉 NOOP 时失败），
  `a_connection_that_died_while_waiting_is_replaced`。
- 文档：`sync.md` 后台同步、`testing.md` 故障列表、`logging.md`。
