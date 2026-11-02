# 测试能看到等账户同步锁的调用

- 日期：2026-11-02
- 类型：重构

## 背景

Issue #209。#205 把 `folders.rs` 的慢窗口换成假服务器上的闸门后，还有几处测试要等"另一个调用已经排在账户
同步锁上"，而这件事服务器那边看不到，只能固定等一段时间：

- `folders.rs` 的 RENAME 测试：spawn `create_folder` 后等 100ms 再放行；等不够就白测。
- `sync.rs` 的 `sync_folder_goes_in_between_the_folders_of_a_full_sync`：500ms 的 `faults.slow`，CI 卡过 500ms 会失败。
- `sync.rs` 的 `a_full_sync_doesnt_skip_a_folder_whose_sync_began_before_it`：500ms 的 `faults.slow`，超时白测。

## 决策

- 账户的两把锁（同步、发送）换成 `AccountLock`：tokio `Mutex` 加一个等待计数，拿到或被取消时减一（drop guard，
  等待的 future 被丢掉也会减）。加一要等到第一次 poll 返回 `Pending` 之后：tokio 那时才把调用排进锁的队列。
  最初是一进 `lock()` 就加，review 指出加完、排队前线程被抢占时，测试已经看到计数并放行，持锁的同步可能
  又先把锁拿回去；在两步之间注入 sleep 能让 `sync_folder_goes_in_between...` 稳定失败。加一用 `Release`、读用
  `Acquire`，看到计数的测试也就看到了排队。
- 拿锁的 future 用 `tokio::task::unconstrained` 包住：任务的协作调度预算用完时，tokio 的 `Acquire` 在
  `coop::poll_proceed` 就返回 `Pending`，还没入队（第二轮 review 指出）。只豁免这一次拿锁，要么拿到、要么入队，
  不会饿着别的任务。计数一直在算，两次原子操作，不值得为它加 `cfg`；
  只有读它的方法放在 feature 后面。发送锁也用同一个类型，免得 `permit` 为两把锁分两种写法。
- 不用 `Arc::strong_count` 推算等待数：只要别处多 clone 一次这个 `Arc` 就悄悄算错。
- 读计数的 `MailClient::sync_waiting` 放在新的 `test-support` feature 后面（GPUI 也是这个名字），
  catmail-core 以自身为 dev-dependency 打开它。考虑过 `#[doc(hidden)] pub`：前端在正式构建里也能调用，
  不如 feature 干净。
- `drafts.rs` 的 `a_draft_that_settles_during_a_sync_is_uploaded_after_it` 原来用 1.5s 的 `faults.slow` 让同步比草稿的
  1s 延迟慢。改成闸门挡住 EXAMINE，等到草稿写完后 1s 再放行。这样 `faults.slow` 没有用处了，删掉，
  只剩闸门一种方式让服务器晚回答。

## 方案

- `crates/catmail-core/src/engine.rs`：`AccountLock`、`Engine::sync_waiting`；`client.rs`：`MailClient::sync_waiting`；
  `Cargo.toml`：`test-support` feature 和自身 dev-dependency。
- `crates/catmail-testkit/src/imap.rs`：删 `Faults::slow`。
- 测试：`folders.rs` 的 `while_held` / `Held` 挪到 `tests/common`（等命令数超过开始时的数，而不是大于 0，
  先前的同步发过同样的命令），新增 `Env::queued_for_sync`（spawn 调用，等到等锁的数比开始时多）；上面
  三个测试改成 `while_held` → `queued_for_sync` → `finish`；草稿测试改用闸门。
- 文档：`docs/architecture/sync.md` 的"锁"一节、`folders.md` 的测试一节。
- `engine.rs` 单元测试 `a_lock_counts_a_call_once_it_is_queued_until_it_stops_waiting`：计数可见时持锁者一放手，
  锁就交给排队的调用；放弃等待后计数归零。按旧的先加后排队、中间注入延迟，它会失败。
  `a_call_out_of_budget_is_counted_once_it_is_queued`：单线程运行时里，排队的任务先用 `coop::poll_proceed` 耗尽预算
  再拿锁；不加 `unconstrained` 时确定地失败。
- 验证：把完整同步的开始时刻挪到拿锁之前（以前修过的 bug），`a_full_sync_doesnt_skip...` 每次都失败（0.05s，
  原来要等 500ms）；改过的测试 72 次并发运行无失败；全部测试、clippy 通过。
