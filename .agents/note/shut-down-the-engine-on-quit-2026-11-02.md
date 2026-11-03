# 退出时显式关闭引擎；Tray / Notifier 从 Resident 取 MailClient

- 日期：2026-11-02
- 类型：重构

## 背景

#212。GPUI 退出时不丢掉全局状态，而引擎要等最后一个 `MailClient` 句柄 drop 才关闭。#201 里
`Resident::shut_down` 因此除了把引擎换成 `Engine::Closed`，还要按名字移除 `Tray`、`Notifier` 两个全局，
因为它们各存了一份 `MailClient`。这是一份特例名单：以后再有全局存 `MailClient`，退出时就会悄悄拿着引擎
不放，只在日志里留一行 `the engine is still open; quitting anyway`。正在等结果的请求（如托盘发起的同步）
也一样拿着句柄。

## 决策

- 根因在"引擎随最后一个句柄关闭"这个机制本身，所以在 core 加 `MailClient::shut_down()`：不管还有谁持有
  句柄都关闭引擎。只在 GUI 里约定"全局不存 `MailClient`"仍要靠人遵守，挡不住进行中的请求，放弃。
- `MailClient` 只剩一个 `Arc<Shared>`：runtime、引擎槽（`Mutex<Option<Arc<Engine>>>`）、`Config`、关闭信号。
  `shut_down` 取走槽里的引擎，之后各克隆的调用得到新的 `Error::Closed`；`config()`、`closed()` 不经引擎，
  关闭后照样能用。`Engine` 不再存 `Config`（只有建图片缓存时用到）。
- 进行中的工作：自建 runtime（GUI）直接 `shutdown_background()`，任务在下一个 await 处被丢掉，连同它们
  持有的引擎引用，调用方得到 `Error::Closed`（`join_error` 把被取消的任务映射成它）。这和最后一个句柄
  drop 时本来就做的一样，等同于进程退出前的状态，发信等的安全性靠的仍是"随时可能被杀"的既有设计。
  取出的引擎在单独的线程（`catmail-close`）上 drop：空闲时退出，关闭（关数据库、可能的 WAL checkpoint）
  就发生在这次 drop 上，不该在 GPUI 主线程；也不能交给 runtime 的 blocking pool，它随即关闭，还没开始的
  任务会在关闭它的线程上被丢掉。
- 数据目录锁在 `Engine` 里，以前任何句柄都拿着引擎，也就拿着锁；现在句柄不再拿着它，所以碰数据目录的
  工作都要自己持有引擎直到做完：`hand_off`（原先完全不经引擎，关闭后还能往别的引擎的数据目录里写）、
  图片缓存的读写清理、`fetch_remote_images` 返回的流的后台任务（原先只拿着缓存）。
  只有 `Runtime::run` / `run_blocking` 把被取消的任务当成关闭（`run_error`）；引擎内部 abort 的任务仍按
  `join_error` 报 `Error::Task`。
  借用的 runtime（CLI）关不了，进行中的工作做完后引擎才关闭，同样与 drop 全部句柄一致。考虑过给每个
  任务加取消信号，但后台任务里有直接 `tokio::spawn` 的（watch、prefetch、keepalive），覆盖不全，放弃。
- GUI 这边仍按 #212 让 `Tray`、`Notifier`、`MainWindow::follow_engine`、`Resident::sync` 从
  `Resident::client` 取：有了 `shut_down` 后这已不是关闭引擎的前提，但引擎状态只有一份，退出后
  （`Engine::Closed`）它们也不再发出注定失败、还会记 warn 的调用。考虑过退回让它们各存一份 `MailClient`
  （少一个 `Resident::hold`），因此放弃。`Resident::shut_down` 不再移除别的全局。
- tray 的绘制测试不经 `Resident::start`（会开主窗口，主窗口的同步会触发重新计数，跟测试手动设的数抢着画），
  加了测试专用的 `Resident::hold`：只放入已打开的引擎。

## 方案

- core：`client.rs`（`Shared`、`engine()`、`shut_down`，方法里 `self.engine.clone()` 改成 `self.engine()?`）、
  `runtime.rs`（`shut_down`，自建 runtime 放进 `Mutex`）、`error.rs`（`Error::Closed`，kind 为 `Conflict`）。
- GUI：`resident.rs`、`new_mail.rs`、`tray/mod.rs`、`main.rs`；`test_support.rs` 加 `header_fetches`。
- 测试：
  - core `tests/shut_down.rs`：自建 runtime 上，被服务器挡住的同步得到 `Error::Closed`，引擎不等服务器
    就关闭（GUI 走的就是这条路）；借用 runtime 上 `shut_down` 后，`hand_off` 也得到 `Error::Closed`（没有检查时失败），其他克隆的调用得到 `Error::Closed`、新订阅
    立即结束；被服务器挡住的同步做完后引擎才关闭，事件流结束，数据目录可以再打开。
  - GUI `quitting_closes_the_engine_whoever_holds_it`：测试自己留着一个句柄，另有一次被服务器挡住的同步，
    退出后引擎仍能关闭，那次同步得到 `Error::Closed`。去掉 `client.shut_down()` 时这个测试失败。
- 文档：`docs/architecture.md`（运行时桥接）、`docs/architecture/background.md`、`docs/architecture/logging.md`。
