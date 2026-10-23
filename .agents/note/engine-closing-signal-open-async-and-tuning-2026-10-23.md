# 引擎关闭信号、open_async 与 EngineTuning

- 日期：2026-10-23
- 类型：重构

## 背景

Issue #43 和 #44。

- 发信 worker 和 GC 只持 `Weak<Engine>`，靠定时器醒来才发现引擎没了。借用 runtime 时（`catmail watch`
  开着 GC），客户端 drop 后它们还要睡最长 30 分钟 / 5 分钟。
- `MailClientBuilder::open` 在调用线程上加锁、迁移、跑两个事务；GUI 在启动时调用它，一次大的迁移会让
  窗口迟迟不出现。
- `Engine::account()` 对前端传来的任何 id 都建 `AccountState`；没有后台同步时 `draft_edits` 没人清理。
- 抓取批大小、回放批大小、整封下载界限是写死的常量，测试和按流量计费的用户想调。

## 决策

- 关闭信号用一个从不发送的 `watch::Sender<()>`，随引擎 drop 关闭，不用另加 Drop 逻辑或新依赖
  （tokio-util 的 CancellationToken）。每账户的后台同步不改：它的 `Cancellation` 监听的 sender 在
  `AccountState` 里，本来就随引擎 drop。
- `open_async` 先建 runtime，再把加锁、迁移和恢复放到 runtime 的 blocking pool；`open` 保留，CLI 和
  测试照用。没有做"只在有待执行迁移时才挪走"：判断本身就要先开数据库。
- GUI 改用 `open_async`：主窗口先打开、留空，打开后换成邮箱，失败时显示原因（原 `OpenFailed`）。
  打开在 `run` 一开始就交给 GPUI 的后台 executor，和应用初始化、建窗口同时进行，tokio runtime 也不在 UI
  线程上建。
- 只读的每账户查询（状态、同步时间、草稿、空闲连接）改成不创建条目；没有后台同步时 `draft_edited`
  不记录（没人会读，也没人清理）。
- `EngineTuning` 只放 issue 说的三项；其余常量不动。批大小小于 1 时按 1 算（`chunks(0)` 会 panic）。

## 方案

- `engine.rs`：`EngineTuning`、`Closing` / `Engine::closing`、`existing()`；`send.rs` / `gc.rs` 的等待
  select 关闭信号；`sync.rs` / `ops.rs` / `body.rs` 改读 `engine.tuning`。
- `client.rs`：`tuning()`、`open_async()`，`open` 拆成 `take_runtime` / `open_engine` / `MailClient::start`。
- `catmail-gui/src/main.rs`：`Startup` 视图。
- 文档：`architecture.md`、`architecture/{sending,sync,bodies,gui}.md`。
- 测试：`background_tasks_end_with_the_client_on_a_shared_runtime`（按 runtime 存活任务数）、
  `open_async_from_a_foreign_executor`、`asking_about_an_account_keeps_nothing_for_it`、
  `drafts_are_not_held_back_without_background_sync`、`new_messages_are_fetched_in_batches_of_the_tuned_size`、
  `the_whole_message_limit_is_tunable`；其中关闭信号、账户条目和草稿记录三个测试在去掉修复后失败。手动启动 GUI 确认正常打开，以及同一
  数据目录开第二个实例时显示错误。
