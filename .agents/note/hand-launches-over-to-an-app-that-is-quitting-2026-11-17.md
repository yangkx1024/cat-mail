# 应用退出时再次启动，新进程接手成为应用

- 日期：2026-11-17
- 类型：修复

## 背景

#325（来自 #319 的全仓库审查）。应用退出时，主循环结束到进程退出之间有 MCP 收尾（最多 5 秒）和引擎关闭。
这段时间里再启动（退出后立刻重开，或重启脚本 `pkill -TERM catmail-gui; catmail-gui`），结果都不对：

- 接收端已经丢掉：答复"it is quitting"，`hand` 报 `HandError::Refused`，新进程报错退出。
- 接收端还活着：`toolkit::spawn` 起的 `answer` 任务在 `toolkit::run` 返回后没人丢掉（GTK 的
  `spawn_future_local`、AppKit 的 `detach` 都是这样），所以实际是这一种。请求进了没人读的队列，
  答复却是成功，新进程以 `Handed` 退出，之后没有应用在运行。

## 决策

- 答复加 `quitting` 字段，`error` 照写"it is quitting"：旧版本的启动照旧报错，而不是误以为交出去了，
  协议版本不用改。新启动收到后不再连 socket，只等锁，最多 30 秒，拿到锁就成为新实例。
- GUI 一确定退出就丢掉请求的接收端（`shell/mod.rs` 的 `quitting(reason)`：`exit`、关掉最后一个窗口而
  退出、第二个信号；主循环结束后再兜底一次）。`quit()` 等写信窗口保存时可能被取消，那时照旧接请求。
  不等主循环结束才丢，是因为退出途中来的请求会再开出主窗口，把退出卡住。
- 接收端放在线程局部变量里，用 `poll_fn` 读：丢掉它不用等 `answer` 任务再被轮询（主循环可能已经结束）。
- 放弃：让 `instance` 返回句柄、用 `close_channel` 从发送端关通道。已排队的请求仍会交给 GUI，得再加
  "已确定退出"的标记把它们忽略，还要改 `Claim` 的接口，并不更简单。
- 放弃：`toolkit::spawn` 在主循环结束时取消所有任务。会改变所有后台任务（持有 `MailClient` 的）的
  释放顺序，而且替代不了确定退出那一刻的调用。
- `Config::lock` 原来每次都建目录、查属主、在数据目录能被其他用户列出时打警告；等锁从最多 1 秒变成
  最多 30 秒（每 50 ms 一次），同一条警告可能打几百遍。拆出 `Config::prepare_data_dir`，加锁前调用
  一次（引擎打开时、`claim_in` 进循环前），`Config::lock` 只开锁文件、试一次锁。
- 仍有的小缺口：确定退出前一刻已经答复成功、但还没被处理的请求会丢。要堵上得等主线程处理完再答复，
  改动更大，没做。

## 方案

- `catmail-frontend/src/instance.rs`：`Reply.quitting`、`Reply::quitting()`；`hand` 返回 `Handing`
  （`Taken`、`Nobody`、`Quitting`）；`claim_in` 收到 `Quitting` 后只等锁，期限 `QUITTING`（30 秒）。
- `catmail-gui/src/shell/mod.rs`：`REQUESTS`、`quitting(reason)`、`stop_answering()`。
- `catmail-core/src/config.rs`：`Config::prepare_data_dir`；`client/builder.rs` 打开引擎时先调用它。
- 文档：`docs/architecture/background.md`（单实例第 1、5 步）、`docs/architecture.md`、
  `docs/architecture/logging.md`。
- 测试：`a_launch_waits_for_one_that_is_quitting_and_takes_over`（对方持锁超过启动期限 1 秒后放开，
  新启动成为实例，之后的启动交给它）；`a_launch_without_quitting_replies_still_reads_one_as_an_error`。
  没在真实应用里验证"退出后立刻重开"。
